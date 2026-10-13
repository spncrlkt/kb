# chord-tool reference

*One page of this, for keeping open while you play:
[CHEATSHEET.md](CHEATSHEET.md). Its hotkey table is checked against this file's by
a test, so the two cannot disagree.*

Every feature and every key, tersely. The narrative version — why anything is
shaped the way it is — is in [README.md](README.md).

This file is **checked against the code**: the hotkey table below is parsed by a
test, resolved through the real keyboard map, and compared with what the keys
actually do. A renamed command or a rebound key fails the build rather than
quietly rotting here.

- [1. The keyboard](#1-the-keyboard)
  - [Chord grammar](#chord-grammar)
  - [Editing hotkeys](#editing-hotkeys)
  - [System and panel keys](#system-and-panel-keys)
- [2. Panels](#2-panels)
  - [Progression](#progression)
  - [Transport](#transport)
  - [Metronome](#metronome)
  - [Sinko](#sinko)
  - [History and Top](#history-and-top)
  - [Synth](#synth)
  - [Ensembles](#ensembles)
  - [EQ](#eq)
  - [Spectrum](#spectrum)
  - [FX](#fx)
- [3. Features](#3-features)
  - [Instruments, placements and ensembles](#instruments-placements-and-ensembles)
- [4. Files](#4-files)
- [5. Limits and defaults](#5-limits-and-defaults)
- [6. Pattern palette](#6-pattern-palette)
- [7. Ensemble and EQ palettes](#7-ensemble-and-eq-palettes)
  - [The instrument palette](#the-instrument-palette)
  - [Roles, and which ones are covered](#roles-and-which-ones-are-covered)
  - [The EQ curve palette](#the-eq-curve-palette)
- [8. What this synth is](#8-what-this-synth-is)
  - [The equaliser, and where it sits](#the-equaliser-and-where-it-sits)
  - [The spectrum, and why it is a filter bank](#the-spectrum-and-why-it-is-a-filter-bank)
  - [Tonewheels, and what the wavetables fixed](#tonewheels-and-what-the-wavetables-fixed)
- [9. The effect rack](#9-the-effect-rack)
  - [The fifteen kinds](#the-fifteen-kinds)
  - [Where the rack sits in the signal flow](#where-the-rack-sits-in-the-signal-flow)
  - [What it costs, measured](#what-it-costs-measured)
  - [What is deliberately not modelled](#what-is-deliberately-not-modelled)
- [10. The voice, expanded](#10-the-voice-expanded)
  - [The filter has five outputs, not three](#the-filter-has-five-outputs-not-three)
  - [Drive, and what it does to the level](#drive-and-what-it-does-to-the-level)
  - [Velocity, which is the accent](#velocity-which-is-the-accent)
  - [The wavetable position blends two spectra](#the-wavetable-position-blends-two-spectra)
  - [Phase distortion](#phase-distortion)
  - [The second oscillator](#the-second-oscillator)
  - [Cross-modulation, which is not phase modulation](#cross-modulation-which-is-not-phase-modulation)
  - [The plucked string](#the-plucked-string)

---

## 1. The keyboard

Three kinds of key, and they never overlap:

- **Chord keys** — the ten home-row positions. They sound, and they are what the
  chord grammar reads.
- **Hotkeys** — the row below the home row, plus `g` on the home row. They act,
  and they deliberately never join the held set, so they stay usable while both
  hands are holding a chord.
- **Panel keys** — `Tab`, the arrows, `Enter`, `Esc`, `Space`, and one modifier
  combination. They drive the UI.

### Chord grammar

Chords are read from the **set** of keys held, not a sequence. Only the ten
home-row positions participate.

**Left hand — scale degree.** `g` is not a chord key; it carries the recall
hotkey.

| Keys (QWERTY keycaps) | Degree |
| --- | --- |
| `f` | I |
| `a` | ii |
| `a` `s` | iii |
| `d` `f` | IV |
| `d` | V |
| `s` | vi |
| `a` `s` `d` | vii |

Any left-hand shape that is not one of these resolves to nothing, and no chord
sounds.

**Right hand — transformation.** Holding `h` selects **h-mode** (diatonic
additions, always in key). With no `h`, **j-mode** (absolute chord qualities,
which may leave the key). Adding `;` to an h-mode shape is invalid by design.

*h-mode* — `h` plus:

| Keys | Suffix | Meaning |
| --- | --- | --- |
| (nothing) | `maj7` | diatonic seventh |
| `j` | `add9` | diatonic ninth |
| `k` | `sus4` | suspended fourth |
| `l` | `6` | diatonic sixth |
| `j` `k` | `maj9` | seventh + ninth |
| `j` `l` | `13` | seventh + thirteenth |
| `k` `l` | `7sus4` | sus4 + seventh |
| `j` `k` `l` | `13(9)` | the full diatonic stack |

*j-mode* — no `h`:

| Keys | Suffix | Keys | Suffix |
| --- | --- | --- | --- |
| `j` | `7` | `j` `k` | `sus2` |
| `k` | `7b9` | `l` `;` | `6/9` |
| `l` | `9` | `j` `l` | `dim7` |
| `;` | `m7b5` | `j` `;` | `7#9` |
| `k` `l` | `m9` | `k` `;` | `aug` |
| `j` `k` `l` | `maj7#11` | `k` `l` `;` | `7#11` |
| `j` `k` `;` | `mMaj7` | `j` `l` `;` | `11` |
| `j` `k` `l` `;` | `13` | | |

A left-hand key with no right-hand key gives the plain diatonic triad. The
suffix is derived from the intervals actually produced, so it is correct in
major and minor without special cases.

### Editing hotkeys

Physical positions, not characters: "Keycap" is the QWERTY label, "You type" is
what reaches the app under Programmer Dvorak. `Shift` selects the *second* action
on a key, never a new key.

<!-- hotkeys:begin -->
| Keycap | You type | Command | What it does | Scope |
| --- | --- | --- | --- | --- |
| `` ` `` | `$` | `SinkoTap` | tap or hold a note into the four-bar loop being recorded | any panel |
| `1` | `&` | `MetronomeToggle` | metronome click on / off | any panel |
| `3` | `{` | `TransportTap` | tap the transport: play/pause, restart, seek to the middle | any panel |
| `4` | `}` | `TransportTap` | the same — the key next door, so a miss still taps | any panel |
| `g` | `i` | `LoadSelectedChord` | recall the chord under the cursor into the registers, and arm the in-place audition | Progression |
| `z` | `'` | `LockRightRegister` | lock the right register | any panel |
| `/` | `z` | `LockLeftRegister` | lock the left register | any panel |
| `x` | `q` | `CopyChord` | copy the selection | Progression |
| `c` | `j` | `PasteChord` | paste the clipboard after the block, or into the gap before position 1 | Progression |
| `v` | `k` | `DeleteChord` | delete the selection | Progression |
| `n` | `b` | `ReplaceChord` | set the selected slot to the chord being held, keeping its rhythm and offset | Progression |
| `b` | `x` | `Undo` | undo the last progression edit | Progression |
| `b` + Shift | `X` | `Redo` | redo the last undone progression edit | Progression |
| `x` + Shift | `Q` | `CopySinko` | copy one rhythm per selected chord to the sinko clipboard | Progression, Sinko |
| `c` + Shift | `J` | `PasteSinko` | lay the sinko clipboard across the selection, in order and repeating | Progression, Sinko |
| `y` | `f` | `HistoryRecall` | put the selected log row back in the registers | History |
| `u` | `g` | `HistoryBack` | step back through the log, sounding each chord for 200 ms | History |
| `i` | `c` | `HistoryForward` | step forward through the log the same way | History |
| `o` | `r` | `HistoryPlay` | sound the selected row while held, fading over 500 ms when let go | History |
| `p` | `l` | `HistoryView` | cycle the log: away, the exact history, the top list | any panel |
<!-- hotkeys:end -->

`{` and `}` are the only two keys that do the same thing: they are adjacent, and
a fumbled transport tap mid-take is worse than the small redundancy.

### System and panel keys

| Key | What it does |
| --- | --- |
| `Tab` / `Shift+Tab` | next / previous panel |
| `↑` / `↓` | move the row cursor |
| `←` / `→` | adjust the selected value, pick the column on a two-dimensional panel, or pick a side of the `[MIDI]` chooser |
| `Shift+←/→` | the coarse step, or the value nudge on a two-dimensional panel |
| `Shift+↑/↓` | extend the chord selection (Progression only) |
| `Enter` | the panel's own action: commit the held chord, edit a value, run a menu item |
| `Ctrl+Enter` | always commit the held chord, appending it, wherever the cursor is |
| `Space` | latch both registers — press twice to clear |
| `Esc` | cancel an open editor or prompt; otherwise stop the transport, and a second press within 500 ms quits |
| `Cmd+A` / `Ctrl+A` | select every chord (Progression only) |

The coarse step is ten bpm on `bpm`, five entries at a time in the pattern list
and on the EQ `preset` row, six key choices on `track key`, four rungs on
`offset`, four bands at a time on the EQ `band` row, and three decibels on the EQ
`gain` row. Everywhere else `Shift` is the plain step, because a toggle is a
toggle.

**While the metronome panel is open it takes every key**, including `Esc`, which
closes it rather than stopping the transport.

---

## 2. Panels

`Tab` cycles: **Progression → Transport → Sinko → Loops → Synth → Ensembles →
EQ → Spectrum → FX**. `↑`/`↓` move the cursor, `←`/`→` adjust, `Enter` acts.

Nine stops, three blocks. The first two are side by side — Progression on the
left, Transport on the right, with the **metronome** panel able to take
Transport's place. The second block is full width and is borrowed rather than
added to: it draws **Sinko**, or the **chord log** when the log is up. The third
is the shared **Synth slot**, which shows whichever of **Synth**, **Ensembles**,
**Loops**, **EQ**, **Spectrum** and **FX** has focus. So `Tab` walks down the
screen and each stop is one panel closer to the bottom, but you never see the
synth table and the equaliser at once: they are views of one panel, which is what
keeps every other view shorter than the tallest of them.

### Progression

The chord list, and the selection every progression action works on. A chord
sounds in red while the loop is on it; the cursor is bold yellow, the rest of a
selected run plain yellow. The header reports `undo: yes` / `redo: yes` and
`N selected`.

| Element | What it shows |
| --- | --- |
| a row | chord label, then its rhythm pattern and offset when it has either |
| the gap above the first chord | an orange rule; the cursor can sit here, which is the only way to paste at position 1 |
| the menu column | `replace`, `reverse`, `rotate`, `clear` — **only while a run is selected** |

**Selection.** Plain `↑`/`↓` selects exactly the row it lands on. `Shift+↑/↓`
extends a run from where the cursor was, so three presses down from the first
chord selects four. `Cmd+A` selects everything. Collapsing the run back to one
chord takes the menu away with it.

**Position 1.** `←`-past-the-first-chord is not a wrap: `↑` from the first chord
moves the cursor into the gap above it, and `c` there pastes at the front.
`Enter` in the gap adds the chord you are holding at the front. Nothing else acts
there.

**The menu** appears with a run. `→` moves the cursor into it and `←` comes back;
`↑`/`↓` walk the items; `Enter` runs one, *even while a chord is held*.

| Item | What it does to the selection |
| --- | --- |
| `replace` | sets every selected slot to the register chord, each keeping its own rhythm and offset |
| `reverse` | turns the run around in place |
| `rotate` | rolls the run one place, last chord to first; repeating it walks the phrase round |
| `clear` | takes the rhythms **and** offsets off, leaving the chords |

`replace` **also has a key of its own** — `n`, which types `b` on Programmer
Dvorak. It is the one item here that a single slot can use: one slot is a range of
one, and the menu only opens once a run is selected, so without the key there
would be no way to overwrite the chord you are sitting on. `reverse`, `rotate` and
`clear` stay menu-only, because none of them means anything applied to one slot.

Every group action is **one** undoable edit, however many chords it touched.

### Transport

| Row | `←`/`→` | `Enter` |
| --- | --- | --- |
| `bpm` | one bpm, or ten with `Shift` (40–240) | type a value |
| `loop` | on / off | on / off |
| `metronome` | click on / off | open the metronome panel |
| `playing` | play / pause | play / pause; shows `bar 3/8`, with `*` while auditioning in place |
| `track key` | every key in turn, wrapping: C major, C minor, C♯ major, C♯ minor, D major… (24 choices, six at a time with `Shift`) | edit the key; in the editor `↑`/`↓` step a semitone keeping the mode |
| `master volume` | one step, 0–7 | edit the value |
| `[MIDI]` | — | open the chooser, then run the bracketed side |

The value column is fixed-width and right-aligned, so every value ends on the
same edge. A `[MIDI]` *outcome* is the exception: it is a sentence, so it runs as
far as it needs rather than being clipped to the column.

**`master volume` is the same number as the Synth panel's `master volume`**, not
a second copy of it: one value with two rows, so a level can be set from the
transport without tabbing three panels away. Editing it from either place opens
the same editor.

**`[MIDI]` is two actions in one row.** `Enter` opens it sideways into

```
  ▸ [MIDI] ─ [EXPORT] import
```

and there `←`/`→` pick a side — the bracketed one is what a second `Enter` runs.
`Esc` closes it again, and so does `↑`/`↓`, which folds it up on the way past. It
was two rows before; the transport is a column of settings and those were the
only two rows doing the same kind of thing.

**What survives a restart.** The key, the tempo and the master volume are written
to `settings.toml` a moment after they stop moving, and read back at start-up. The
progression is *not*: that is a document rather than a setting, and there is
already a way to keep one — export it.

### Metronome

Opened with `Enter` on the transport's `metronome` row; covers the Transport slot
and closes on `Esc`. Shares the transport's shape, so opening it does not change
the column's width.

| Row | Values |
| --- | --- |
| `click` | on / off (`Enter` or the arrows) |
| `sound` | `Blip`, `Tick`, `Wood`, `Beep`, `Two Tone` — generated, not sampled |
| `volume` | 0–100%, applied on the click's own voice |
| `subdivision` | 1, 2 or 4 clicks per beat |
| `swing` | 0–100% in 5% steps: the default every pattern without its own follows |

### Sinko

The rhythm editor. It has **no cursor of its own**: it always describes the chord
the Progression panel has selected. Every row writes through to that chord
immediately — nothing to save, nothing on disk.

| Row | `←`/`→` | Notes |
| --- | --- | --- |
| `chord` | step the Progression selection | clamped, and a rest is a stop like any other |
| `pattern` | cycle `(none)` then the palette | shows `[8/19]`; `Shift` jumps five; `(edited)` when the chord's bar has drifted from the library. Assigning a **four-bar** pattern spreads its bars across this chord and the three after it |
| `offset` | walk the note ladder | −whole to +whole note, independent of the pattern |
| `quant` | 2, 4, 8, **12**, 16, **24**, 32, 64 steps per bar | 12 and 24 are the triplet grids; holds are in ticks, so they survive a grid change, and the hits are re-quantized onto the new grid rather than dropped |
| `swing` | `follow transport`, then 0–100% | this pattern's own groove |
| `hits` | walk the cell cursor; `Enter` toggles the cell | the grid is drawn under this row rather than as a row of its own |
| `length` | the hit under the cursor: `default` then the note ladder | per-hit override |
| `accent` | the hit under the cursor: 20–100% | per-hit level |
| `hold` | the default length for hits without an override | 1 tick to a whole bar |
| `mute` | none, then up to a quarter note | silences the end of the *bar*; anything ringing in is cut at the boundary |
| `loop` | `Enter` opens the Looper on this chord | the four-bar tap recorder |
| `[New Pattern]` | `Enter` | give the selected chord a blank rhythm of its own, named and audible at once |
| `[Save Pattern As...]` | `Enter` | copy the chord's bar into the palette under a new name (`Name 2` if taken) |
| `[Copy Sinko]` | `Enter` | one bar per selected chord to the sinko clipboard |
| `[Paste Sinko]` | `Enter` | names what is waiting, e.g. `[Paste Sinko: 2 rhythms, 1 plain]` |

### History and Top

The log of what has been played this run, in two views. It borrows the rhythm
panel's slot, so it is drawn whether or not that panel has focus, and the chord
list and the readout above it stay visible.

| ⌨ | What it does |
| --- | --- |
| `l` | cycle the log: away, the exact history, the top list, away. Works from any panel |
| `g` / `c` | step back / forward through the list, sounding each destination for 200 ms |
| `r` | hold to sound the selected chord, release to let it fade over 500 ms |
| `f` | put the selected chord back in the registers |

The **history** is every play in order, numbered `#1` upward, duplicates included;
the cursor opens on the newest, which is the one you just played. The **top** list
is the same plays deduped by chord and ranked by count, ties broken by which was
played most recently, with the count in place of the row number. In both, a chord
that appears anywhere in the progression is marked `●` and coloured green.

```
━━ History [9 played · 5 chords] ━━
       #  chord      deg   notes
      #1  C          (I)   C4 E4 G4              ●
  ▸   #5  Dm9        (II)  D4 F4 A4 C5 E5        ●
  f put in the registers   g/c move   r play while held   l hide   ● in the progression
```

**A play is a chord held for at least 150 ms** while it is audible, which is what
collapses the two halves of one two-handed chord into one entry: the audition
speaks the instant a chord changes, so a left hand followed by a right hand sounds
the triad and then the shape you meant, and only the second was played. A chord
held while the loop runs with no in-place audition armed is silent, and silent is
not played. Duplicates in the history mean separate holds.

**The 200 ms is the movement's own**, and the next movement takes the voice over
rather than stacking on it, so walking twenty rows is one chord at a time. `r`'s
fade is the audition's own half-second release — the same release a chord held and
released on the keyboard gets, because it is the same voice.

The log is **per run**: nothing writes it anywhere, and closing the app ends it.
While it is up the rhythm panel behind it does not answer its own keys — a key
that acts on a row you cannot see is the one thing the panel scoping exists to
prevent — and `l` gives them back.

### Synth

The sound design: three channels (`low`, `mid`, `high`) side by side as columns,
with a master block underneath. There are three dozen settings per channel, so the
panel is **paged** — seven screens, one keystroke apart. Every page is padded to
the same height, so paging never moves the panels below it.

| ⌨ | What it does |
| --- | --- |
| `PageDown` / `PageUp` | next / previous page, wrapping |
| `↑` / `↓` | row within the page |
| `←` / `→` | which column: `low`, `mid`, `high` |
| `Shift+←` / `→` | change the value |

The page is named in the panel title — `Synth [filter 5/7]` — because a paged
table whose page you cannot see has settings you will never find.

| Page | Rows |
| --- | --- |
| `tone` | `volume`, `waveform`, `pulse width`, `position`, `phase dist`, `noise level`, `transpose`, `pan` |
| `osc` | `osc2 waveform`, `osc2 interval`, `osc2 level`, `osc2 fm`, `fm mode`, `feedback`, `osc2 ring`, `glide` |
| `pluck` | `pluck decay`, `pluck damp`, `pluck burst` |
| `env` | `attack`, `decay`, `sustain`, `release`, `env curve` |
| `filter` | `cutoff`, `resonance`, `filter type`, `drive`, `filter env`, `filter attack`, `filter decay`, `key track` |
| `mod` | `lfo pitch`, `lfo cutoff`, `lfo amp`, `lfo pwm`, `vel cutoff`, `vel pwm`, `unison`, `detune` |
| `fx` | `reverb send`, `delay send`, `slot 1` … `slot 6` |

The **`fx` page** is the rack seen from above: the two sends, then one row per
insert slot, each showing what that register has in it. `Shift+←`/`→` on a slot
row swaps the whole family — the fast way to find out whether a rack wants a
phaser at all — and `Enter` opens the FX panel on that slot to edit it properly.

The first row is **`instrument`**, and it is the one to know about. It shows, per
register, which library instrument that register was loaded from — or `custom` if
it was never loaded from the library at all, or a leading `*` if it has been
changed since. `Shift+←`/`→` on it steps through the library **loading each into
that register live**, so the way to audition instruments is to hold `Shift` and
tap an arrow with the loop running.

`Enter` opens a picker that opens on whatever the register already is, where
`↑`/`↓` auditions (with `Shift` for five at a time), `Enter` keeps and `Esc` puts
the register back exactly as it was. Press **`s`** in the picker to keep whatever
the register is holding as a **new instrument**: it asks for a name, writes it to
`instruments.user.toml`, and loads it back, so the register is named from then on.
The name is offered as `<the instrument it was> 2` rather than the same name, so
keeping a tweak adds to the library instead of replacing the thing you tweaked.

The whole orchestra is saved the other way: `[Save As...]` on the Ensembles
panel captures all three placements plus the mixer as an ensemble. So the loop
is: swap instruments until it sounds right, then save the ensemble. Any register
you dialled in by hand rather than loading from the library is kept as a new
instrument named after the ensemble, so nothing is lost by saving.

The marker leads rather than trails because the column is only wide enough for a
dozen characters: `Rhodes Dark (edited)` would be clipped to `Rhodes Dark (e`,
which cuts off the one thing the marker exists to say.

Swapping an instrument writes two dozen parameters at once. Nothing is
reallocated and no voice is retriggered, so a swap never interrupts a note that is
already sounding.

The `waveform` row cycles twenty-four shapes: `sine`, `saw`, `square`,
`triangle`, `noise`, `pluck`, then eighteen **wavetables** — four tonewheel registrations
(`organ full`, `organ jazz`, `organ bright`, `organ hollow`), the pipe-organ
spectra (`principal`, `gedeckt`, `reed`), the wind spectra (`clarinet`), the
struck and plucked spectra (`piano`, `vibes`, `nylon`), the vowels
(`vox aah`, `vox ooh`) and the earlier character tables (`metallic`, `vox`,
`glass`, `mellow`, `buzz`). Twenty-three is a long list for one row, which is
what the coarse step is for: inside an open edit `↑`/`↓` step five shapes at a
time, so any of them is at most five presses away.

Underneath, on **every** page, is the master block: first the two **aux units** —
the reverb and the delay every register sends to — then `master volume` /
`master mute`, `lfo rate` / `lfo wave`, and `note length`.

| Row | What it sets |
| --- | --- |
| `subtype` / `preset` | which variant of the unit, and which stored setting of that variant |
| `size`, `damp`, `predelay` | the reverb itself |
| `reverb level` | how much of the reverb return you hear |
| `time`, `feedback`, `tone`, `sync`, `division` | the delay itself; with `sync` on, `time` follows the tempo and `division` picks the note value |
| `delay level` | how much of the delay return you hear |

Both units are **fully wet** — their own `mix` parameter is not exposed, because
the return level is what decides how much of them you hear, and one number doing
that job is better than two. `preset` walks the library for the variant on screen
and reads `custom` once a parameter has moved. Those two rows, the master volume
and note length are genuinely global, so they belong to no page in particular —
and pinning them keeps the effects and the master volume reachable from wherever
you are.

`Enter` opens an edit: the arrows adjust it *live* while you hear the result,
`Enter` keeps it, `Esc` puts the old value back. Editing with a chord held needs
`Shift`+arrows, because `Enter` still commits the chord first.

### Loops

The four-bar tap recorder. It loads a bed of four bars, loops it with the click,
and records `$` into it — then saves all four bars as one pattern, which Sinko can
assign like any other. It borrows the Synth slot, so it costs the layout no rows
of its own. Reach it with `Tab`, or with `Enter` on Sinko's `loop` row, which
opens it on the chord that panel is describing.

**All four bars are on screen at once**, one grid per line, under the `hits` row:
the loop is the thing being made, and a bar at a time hides its shape. The bar the
cursor is on carries the edit cursor and the row band; the bar the clock is on
carries the playhead. `x` is a hit, `·` a rest and **`=` a cell a hit is still
sounding through** — the hold, drawn — so a note held across a beat reads as a
line rather than a dot.

| Row | `←`/`→` | Notes |
| --- | --- | --- |
| `source` | `one chord, four times` / `the first four chords` | what `[Record]` loads |
| `chord` | pick the progression row `one chord` means | the label is the chord |
| `bar` | 1–4, wrapping | which bar the cursor, `length`, `accent` and `[Clear Bar]` act on |
| `quant` | the same ladder as Sinko | steps per bar for a bed that has not been loaded yet |
| `hits` | walk the cell cursor; `Enter` toggles the cell | the four grids are drawn under this row |
| `length`, `accent`, `hold` | the same as Sinko | per-hit and per-bar, on the bar the cursor is on |
| `[Clear Bar]` | `Enter` | empties the bar the cursor is on, and only that bar |
| `[Record]` | `Enter` | three states, cyclically: nothing loaded → load and loop; recording → stop recording but keep the loop; loaded and stopped → unload and hand the progression back |
| `[Save Loop As...]` | `Enter` | all four bars into the palette as one pattern |

**`$` records a note, not a dot.** What it captures is the key's own down and up:
the press is the note's start — landing in **the bar the clock is on**, and
sounding *that* bar's chord as it is placed — and the key-up is its end. The
distance between the two becomes the hit's **length**, rounded to the bar's grid;
a tap that begins and ends inside one cell is an ordinary hit and keeps the bar's
default hold. A note held over a bar line keeps sounding into the next bar, up to
a bar long, and the cells it was held through are cleared, so a hold replaces the
taps under it rather than layering with them. Because the tap is a key and not a
click, holding it is one note: the terminal's auto-repeat is absorbed rather than
read as a burst of new notes.

On each pass the recorded hits play back with their own bar's chord. `Esc` stops
the transport and the recording but keeps the four bars — losing work is not
something a panic key should do — and it hands the tap's voice back on the way.

### Ensembles

One row per **ensemble** you can load — the seventy-four shipped ones and yours,
with yours winning wherever the names collide — then `[Save As...]`. `Enter` loads
an ensemble: its three placements are resolved against the instrument library, its
mixer travels with it including note length, and the instrument row is then named
for all three registers at once, because an ensemble says what each one is.
`Enter` on the last row opens a name prompt and writes the current sound to
`ensembles.user.toml`. Each placement's thirteen-band EQ travels with it, and so
does the master curve, because both belong to the placement and the mixer rather
than to the instruments.

### EQ

The thirteen-band graphic equaliser: one curve at a time, for the three registers
and the mix. It borrows the Synth panel's slot, so opening it hides the synth
table — and the panel's title says which equaliser you are looking at.

| ⌨ | What it does |
| --- | --- |
| `↑` / `↓` | row: `target`, `preset`, `band`, `gain`, `[Save Curve As...]` |
| `←` / `→` on `target` | cycle `low`, `mid`, `high`, `master` |
| `←` / `→` on `preset` | step through the curve library and **apply the whole curve**; `Shift` is five at a time |
| `←` / `→` on `band` | move the band cursor; `Shift` is four bands |
| `←` / `→` on `gain` | that band, ±0.5 dB; `Shift` is ±3 dB |
| `Enter` on `gain` | return that band to 0 dB |
| `Enter` on `[Save Curve As...]` | name the curve on screen and write it to `eq_presets.user.toml` |

```
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

The thirteen bands are `20 31.5 50 80 125 250 500 1k 2k 4k 8k 12.5k 16k` Hz: two
thirds of an octave apart at the bottom, an octave through the middle and closer
at the very top, because the audible octaves below 125 Hz are few and heavy and
that is where a twelve-band graphic runs out. The bottom band is a low shelf, the
top is a high shelf, and the eleven between them are bells. Each band spans
±12 dB. A cut grows to the left of
the centre rule and a boost to the right, one cell per decibel, with the exact
value printed at the end of the row; the band the cursor is on is marked `▸` in
the label column, and the row it is on is highlighted when the `band` row has the
row cursor.

`preset` is **derived** from the thirteen gains rather than remembered, so it can
never name a curve that is not on the screen: `Flat` when the curve is bypassed,
a library name when the curve matches one exactly, `custom` otherwise. Pressing
an arrow on `custom` starts at the beginning of the library, which is `Flat` —
the one curve that is a reset.

The curve belongs to the **placement**, not the instrument: swapping the
instrument in a register leaves its curve exactly where it was, and moving an EQ
band never makes the `instrument` row show its `*` marker, because the sound has
not changed — only where it sits. `master` is applied to the finished stereo pair
after the reverb, so it is the tone control for the whole instrument; the three
register curves are applied before that register's fader, so they shape what goes
to the reverb as well as what is heard.

### Spectrum

The live spectrum: thirteen band levels, for one register or for the mix. It is a
**readout** — nothing on it changes a sound — so it is the one panel that shares
another's cursor: `target` is the EQ panel's own, and tabbing between the two
keeps you on the same part.

It is also the only panel that costs nothing while it is off screen. The thirteen
bands are thirteen bandpass filters behind each of the four taps, run inside the
audio callback, so the interface tells the callback when the panel is not the one
being drawn and the filters are skipped. Coming back to it clears the peaks and
starts from the sound that is playing, rather than drawing a level from whenever
you last looked.

| ⌨ | What it does |
| --- | --- |
| `↑` / `↓` | row: `target`, `range`, `speed`, `hold`, `[Reset Peaks]` |
| `←` / `→` on `target` | cycle `low`, `mid`, `high`, `master` — the same cursor the EQ panel uses |
| `←` / `→` on `range` | 48, 60 or 72 dB from the top of the chart to the bottom |
| `←` / `→` on `speed` | envelope release: `fast` 48, `medium` 24, `slow` 8 dB per second |
| `←` / `→` on `hold` | toggle the held peaks |
| `Enter` on `[Reset Peaks]` | drop every held peak |

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

Each of the twelve chart rows can be half full, so the picture has twenty-four
levels of resolution down its span — two and a half decibels a step at 60 dB. The
bands are **the EQ's own ladder**, so a level and the curve shaping it are read on
the same thirteen columns. Full scale is at the top: the master tap is taken after
the soft clip, so nothing there can exceed it, while a register tap is taken
before the clip and so pegs at the top rather than being hidden.

`speed` is the envelope release and nothing else: attack is instant, because a
meter that ramps up misses the transient it exists to show. A fast release shows
the *rhythm* of a part and a slow one its *balance*, which is why it is a row
rather than a constant. `hold` draws the peaks the panel has been keeping as a
`▀` tick above each bar; they are held by the panel, not by the audio thread,
because the panel reads all fifty-two published levels every frame and so never
misses one.

The three register taps are taken **after that register's curve, its effect rack
and its fader**, so a register reads what it contributes: mute it and its meter
goes quiet, a distortion shows up as harmonics, pan it hard and it still reads its
own level. The master tap is the finished pair, after the
master curve, the master gain and the clip.

### FX

One register's **insert rack**, opened out: six slots, and the one under the
cursor in full. The rack, the slot, the kind, the variant and every parameter are
here, which is why the panel is the one view of the Synth slot that is not a
table of the three registers at once.

| ⌨ | What it does |
| --- | --- |
| `↑` / `↓` | row |
| `←` / `→` | the value on that row |
| `Shift+←` / `→` | the coarse step, where a row has one (a parameter that spans two orders of magnitude) |
| `Enter` on `[Move Earlier]` / `[Move Later]` | slide the slot's effect one place along the rack |
| `Enter` on `[Empty This Slot]` | set it to `none` |
| `Enter` on `[Save Effect As...]` | name it and write it to `fx_presets.user.toml` |

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

`rack` cycles `low`, `mid`, `high`; `slot` cycles the six places in that rack.
Changing either resets the `param` cursor, which belonged to the slot you were
on. The title names the rack, the slot's place in it and the whole effect.

`type` cycles the fifteen kinds, loading the new kind's own defaults — a kind
that kept the last kind's numbers would be a rename, not a sound. `subtype` does
the same within the kind. `preset` walks the library for the kind and
variant on screen, and reads `custom` as soon as a parameter moves.

Six parameters is more than a fixed row list can hold without the rows moving
whenever the kind changes, so they are reached through a **selector**: `param`
chooses which one — `2 of 4  tone` — and `value` moves it. It is the same pair
the EQ panel uses for its thirteen bands.

`[Move Earlier]` and `[Move Later]` **swap** the slot with its neighbour, and the
cursor follows the *effect*: the reason to move a distortion is to put it in front
of the chorus, and afterwards it is still that distortion you are editing. The
ends of the rack refuse rather than wrapping — a rack is an order, not a wheel.

The rack is per register and lives here. The **aux units** — the reverb and the
delay that every register sends to — are master settings and live in the Synth
table's master block, on every page, beside the return levels that decide how much
of them you hear. Two places, because they are two different things: an insert
belongs to one register, and the send belongs to the mix.

---

## 3. Features

### The chord readout

Above the chord list: the resolved chord's name, its scale degree relative to the
track key, and its notes — `F7 (V)`, `Dm (ii)`, `Bdim (vii)`. It reflects a
latched register exactly as the audio and `Enter` do.

### Registers

`Space` latches both hands at once; `'` latches the right, `z` the left. A latch
captures whatever that hand is holding. Afterwards live input wins **per side**,
so one hand can override its latch while the other keeps filling in. Pressing
`Space` with nothing held captures *empty* for both sides, which makes it a
toggle: twice clears.

### Auditioning

There is no "live bar" at the end of the loop. What you are playing — latched
registers plus whatever is under your hands, live winning per side — is the
**computed chord**, and it is heard two ways:

- **Stopped, it is the instrument.** Every change speaks the instant it happens,
  with no wait for a bar line at the current tempo. The note holds while the
  chord holds, then rings for 500 ms rather than being cut. A new chord inside
  that window takes the note over instead of stacking, so a run of changes stays
  legato. It has a voice of its own, so trying a chord can never retrigger or cut
  one the loop is playing.
- **Playing, a change is heard in place.** Press `g` on a chord to recall it —
  that arms the audition — and then modify it: while the loop runs, the computed
  chord stands in for *that slot*, keeping its rhythm, offset and place in the
  loop. The `playing` row marks it with a `*`. Moving the selection, or editing
  the progression, exits it.

### Rhythm patterns

A chord can play a pattern instead of one whole-bar stab. Patterns are per-chord
**copies**: two chords can both start from `Quarters` and drift apart. A pattern
is **one to four bars**; each bar is a grid (2–64 cells, including the triplet
grids 12 and 24), a default hold in ticks, per-cell length and accent overrides, a
muted tail and an optional swing override.

Tapping builds the grid a note at a time: `$` records into the Loops panel's
four-bar bed, and what it records is the key's down and up — the start and the end
of a note, so the length the key was held for becomes the hit's hold. A tap inside
one cell is a plain hit. Hand-editing is the Sinko `hits` row, and a key held down
is one note: auto-repeat is absorbed, and taps are resolved 300 ms after the last
press.

### Multi-bar phrases

A four-bar pattern is a **phrase**: assigning one spreads its bars across that
chord and the three after it, in one undoable edit. `Jazz Chorus` is one such
entry and `Son Clave` is a two-bar one — they used to ship as one-bar fragments
whose *names* carried the position (`Jazz Chorus 1/4` and so on), which is the
same musical idea with the contract in a string. `AssignedRhythm` records which
bar of the phrase a chord holds, so any single bar can still be edited, offset or
muted on the chord it lands on.

Four bars is the ceiling because the Loops panel's bed is four bars — it is what
`$` can record in one pass of the transport. A phrase is **data, not a mode**: it
is a `RhythmPattern` in the library, it is written into the session document bar
by bar, and `[Save Loop As...]` is the same thing arriving from the recorder.

### MIDI export and import

`[Export MIDI]` on the transport writes a Standard MIDI File (960 ppq) to
`progressions/`, named by timestamp, with the whole session embedded as a
project chunk. `[Import MIDI]` reads one back and restores the progression, the
track key, the tempo, the note length and any rhythm patterns it carries, in one
undoable step. The computed chord is never exported: a file is a function of the
progression alone.

### Instruments, placements and ensembles

Sounds live in three levels, which is the thing to understand before anything
else here:

| Level | What it is | Where it lives |
| --- | --- | --- |
| **Instrument** | one sound, with no level, pan, transpose or curve of its own | `instruments.toml` |
| **Placement** | a named instrument plus where it sits: level, transpose, reverb send, pan, and a thirteen-band EQ curve | inside an ensemble |
| **Ensemble** | three placements — `low`, `mid`, `high` — and the mixer, which has a curve of its own | `ensembles.toml` |

The split is not bookkeeping. The three registers are **three parts**, and the
chord is voiced across them: `low` takes the root an octave below, `mid` takes the
chord, and `high` takes the top note an octave above. That is what the block sizes
are for — one note for the low, four for the mid, one for the high. An instrument
that carried a register's level and transposition could not be moved between them
without dragging those along, and keeping a voice register-neutral is what lets
`Celesta` — Marimba's top register, an octave up — be dropped into the bass and
still be a celesta.

**A bass, a chord and a descant.** The voicing is worth stating because it was
wrong in a way nothing caught for a long time. The registers used to be handed the
chord's *tones* at their written pitch — the low took the bottom note, the high the
top, the mid whatever was left, and only a single note was spread by an octave. A
triad on middle C therefore came out as C4, E4 and G4: three registers inside a
major third, which the ear fuses into one instrument however different the three
patches are. The names were a lie for every chord, and
`a_triad_is_voiced_as_a_bass_a_chord_and_a_descant` is the assertion that says so
now.

Past four notes the top is left to the descant and the rest of the chord goes in
as before, rather than the middle block being filled until it overflows. The mid
block holds four notes, and the low and high are already doubling the ends, so a
fifth or sixth tone that did not fit would be silent everywhere — splitting it
this way keeps every pitch class sounding. A ninth comes out as the root an octave
down, the third, fifth and seventh in the middle, and the ninth an octave up.
Percussion does not fit this model at all, because a drum has no register; the
three kit ensembles are the one place the names stay approximate.

It also means **auditioning an instrument does not disturb your mix**: swapping a
sound changes the two dozen parameters that say what it is, and leaves the
register's level, pan, transpose, reverb send and EQ curve exactly where you set
them.

The three files ship with the build and are tracked; your own versions are the
matching `.user.toml` files, gitignored, created empty on first run, and layered
over the shipped ones by name. Saving never overwrites a name — `Name 2` if it is
taken, except on the EQ `preset` row, where the name is only a label and the
curve is a copy.

---

## 4. Files

| File | Tracked | What it is |
| --- | --- | --- |
| `ensembles.toml` | yes | The seventy-four shipped ensembles, compiled in with `include_str!`. Editing the file **is** how the palette changes — there is no Rust to keep in step. |
| `ensembles.user.toml` | no | Created empty on first run. What `[Save As...]` writes, layered over the shipped ensembles by name. |
| `instruments.toml` | yes | The one hundred and fifty-five shipped instruments, compiled in the same way. Register-neutral voices, each with a name. |
| `instruments.user.toml` | no | Created empty. What the picker's `s` writes. |
| `eq_presets.toml` | yes | The twenty-five shipped EQ curves, compiled in the same way. |
| `eq_presets.user.toml` | no | Created empty. What `[Save Curve As...]` writes. |
| `fx_presets.toml` | yes | The seventy-two shipped effect presets, compiled in the same way. |
| `fx_presets.user.toml` | no | Created empty. What the FX panel's `[Save Effect As...]` writes. |
| `rhythms.toml` | yes | The nineteen shipped patterns. The odd one out: Rust is the source and this file is regenerated from it, so the file and the built-ins are checked equal by a test. A pattern is written as one `[[rhythms.bars]]` table per bar — one for nearly all of them, four for a phrase. |
| `rhythms.user.toml` | no | Created empty. What `[Save Pattern As...]` writes. |
| `progressions/` | no | Where MIDI exports are written and imported from, created on first launch. Falls back to the working directory if it cannot be made. |
| `settings.toml` | no | Written a moment after the key, tempo or master volume stops moving, and read back at start-up. Not created until one of them changes. A missing or unreadable file silently means "the defaults"; only a file that exists and will not parse is reported. |
| `debug.log` | no | Truncated and rewritten on every launch: every input event (`[IN]`), a 60 Hz output-level tap (`[OUT]`) and, once a second, the audio timing (`[TIME]`), so it grows fast. |
| `REFERENCE.md` | yes | This file. Its hotkey table is parsed by a test and resolved through the real keyboard map. |

Every library's `.user.toml` is created empty on first run and is the only half
the program ever writes; the tracked file beside it is read-only at runtime, so a
fresh checkout never has to be told where the library is.

### Environment

| Variable | Effect |
| --- | --- |
| `CHORD_TOOL_TIMING` | Anything non-empty turns on the named timing scopes: the scheduler's per-bar planning, the draw loop, import, export and start-up are measured, and the last second's distribution for each goes into `debug.log` as part of the `[TIME]` line. The callback's own counters — its share of the buffer deadline, its worst buffer, how many buffers missed — are always on, because a diagnostic that has to be enabled before it can be asked for is not there when the question is asked. |
| `CHORD_TOOL_RENDER` | Test-only. A path — a file, or a directory when it has no extension — that every offline render writes itself to as 16-bit stereo WAV. How a changed fingerprint gets listened to rather than argued about. |
| `NO_COLOR` | Honoured by crossterm, then deliberately overridden: the panels use weight and colour together, and `NO_COLOR` is a convention for piped text rather than for a full-screen instrument. |

---

## 5. Limits and defaults

| Thing | Value |
| --- | --- |
| Tempo | 40–240 bpm, default 120 |
| Persisted | the track key, the tempo and the master volume, in `settings.toml` |
| Chord log | per run, in memory only; a play is a chord held 150 ms while audible; movement-cued chords sound for 200 ms; `r` fades over the audition's 500 ms |
| Track key | 12 tonics × major/minor, tonic pinned to MIDI 24–95 (C1–B6) |
| Bar | 4/4, 960 ppq, 3840 ticks |
| Grids | 2, 4, 8, **12**, 16, **24**, 32, 64 cells per bar |
| Hold | 1 tick … one whole bar (3840); default 240 |
| Muted tail | 0 … a quarter note (960) |
| Offset | ± one whole bar (3840) |
| Pattern length | 1 … 4 bars; a four-bar pattern is a phrase and spreads across four chords when assigned |
| Looper bed | 4 bars, from one chord or the first four of the progression |
| Undo history | 128 edits |
| Audition release | 500 ms |
| Tap resolution | 300 ms after the last press; taps closer than 30 ms are key repeat |
| Per-channel ranges | volume 0–7 · noise 0–100 % · attack 1 ms–2 s · decay 1 ms–8 s · sustain 0–100 % · release 1 ms–8 s · curve 0–100 % · glide 0–2 s · cutoff 200 Hz–8 kHz · resonance 0–99 % · filter env ±100 % · filter attack 1 ms–2 s · filter decay 1 ms–2 s · key track 0–100 % · each LFO depth 0–100 % (pitch, cutoff, amp, pulse width) · unison 1–4 · detune 0–50 cents · pulse width 5–95 % · transpose ±24 st · reverb send 0–100 % · delay send 0–100 % · pan L100–R100 · drive 0–100 % · velocity to cutoff / pulse width 0–100 % · position 0–100 % · phase distortion 0–100 % · osc2 interval ±24 st · osc2 level / FM 0–100 % · cross-modulation domain one of `phase` / `linear` / `expo` · feedback 0–100 % · per-voice ring modulation 0–100 % · pluck decay 50 ms–8 s · pluck damp 0–100 % · pluck burst 5–100 % of a period |
| Global ranges | master volume 0–7 · LFO rate 0.05–20 Hz · each aux return 0–100 % · reverb size / damp 0–100 %, predelay 0–120 ms · delay time 1 ms–2 s (ratio-stepped), feedback 0–95 %, tone 0–100 %, sync on/off, division one of thirteen note values |
| Effect racks | six insert slots per register, eighteen in all; **two** aux units, the reverb and the delay; the aux units run fully wet |
| Effect presets | 72 shipped, plus yours; `preset` walks the ones matching the kind and variant on screen |
| EQ | 13 bands — `20 31.5 50 80 125 250 500 1k 2k 4k 8k 12.5k 16k` Hz — each ±12 dB in half-decibel steps; bottom band a low shelf, top band a high shelf, eleven bells at Q 1.414. A curve of any other length reads as flat rather than being stretched |
| EQ curves | 25 shipped, plus yours |
| Spectrum | 13 bands — the EQ's ladder — 4 taps (`low`, `mid`, `high`, `master`); spans 48 / 60 / 72 dB; envelope release 48 / 24 / 8 dB per second; peak hold per band |
| Terminal | 80 × 16 minimum; wider windows give spare columns to the bar grid up to 120 |
| Panel heights | Transport 15, Synth 34, **Sinko 31**, EQ 34, Spectrum 33, FX 25, Ensembles 34, **Loops 29** rows — asserted by a test, because every row added anywhere costs the whole layout. The ensemble list draws a **window of twenty rows** and scrolls, because the library it shows is longer than any terminal |

---

## 6. Pattern palette

Nineteen patterns ship with the build, in this order — the order the `pattern`
row cycles, grouped so that neighbours sound related. A pattern is **one bar or
four**: a one-bar pattern is a chord's rhythm, and a four-bar pattern is a
*phrase* whose bars spread across consecutive chords when it is assigned.

| Group | Patterns |
| --- | --- |
| Sustained | `Held Whole`, `Held 3/4`, `Held Half`, `Two Feel` |
| Straight | `Quarters` |
| Eighth grid | `Eighths`, `Offbeat Eighths` |
| Sixteenth grid | `Offbeat 16ths`, `Dembow`, `Charleston`, `Tresillo`, `Syncopated 16ths`, `Sixteenth Pulse` |
| Triplet grid | `Swung Eighths` |
| Texture | `Damped Quarters` (ships muted), `Accented Eighths` (ships accented), `32nd Roll` |
| Phrases (four bars) | `Jazz Chorus` |
| Phrases (two bars) | `Son Clave` |

`Charleston` and `Tresillo` carry per-cell lengths — a dotted quarter answered by
an eighth, and 3+3+2 — rather than one uniform hit. The phrases used to ship as
one-bar fragments whose *names* carried the position (`Jazz Chorus 1/4` and so
on); a pattern holds its own bars now, which is the same guarantee without the
numbering, and it is what the looper records.

---

## 7. Ensemble and EQ palettes

### The instrument palette

A hundred and fifty-five voices ship, and **eighty-one** of them are distinct
designs — the other seventy-four are the `low` / `mid` / `high` variants an
ensemble needs for a register that sounds an octave away from the same
instrument. A voicing counts as a design of its own: `Rhodes Bright` and `Rhodes
Dark` share a name but are two, which is why neither can be the other's middle.
No control the voice has is unused: a test walks the library and fails if any of
them is.

| Family | Voices |
| --- | --- |
| Bass | `Electric Bass`, `Upright Bass`, `Sub Bass`, `Acid Bass`, `Bass Guitar`, `FM Bass` |
| Keys | `Rhodes` (three), `Wurli` (two), `Clav`, `Grand Piano`, `Honky Tonk Tine`, `Electric Piano`, `FM Tine Piano`, `Dirty Tine`, `Hollow Clav`, `Accent Clav` |
| Plucked | `Nylon Guitar`, `Pizzicato`, `Harp`, `Pluck`, `Steel Guitar`, `Nylon Pluck`, `Harpsichord`, `Muted Guitar`, `Sitar` |
| Organ | `Drawbar Organ`, `Rock Organ`, `Pipe Organ`, `Gospel Organ`, `Gedeckt`, `Twin Registration Organ`, `CZ Organ`, `Overdriven Organ` |
| Mallets and bells | `Vibraphone`, `Marimba`, `Kalimba`, `Bell`, `Glass Bell`, `Celesta`, `FM Bell`, `FM Marimba` |
| Pads and strings | `Warm Pad`, `String Ensemble`, `PWM Pad`, `Vox Pad`, `String Pad`, `Phase Pad`, `Glass Morph Pad`, `Morph Choir` |
| Voices | `Choir Aah`, `Choir Ooh` |
| Winds and brass | `Soft Horn`, `Trumpet`, `Clarinet`, `Oboe`, `Flute`, `FM Brass` |
| Leads | `Buzz Lead`, `CZ Lead`, `Talking Lead`, `Folded Saw` |
| Cross-modulated | `Growl Bass`, `Through-Zero Bass`, `X-Mod Clang`, `X-Mod Organ`, `Ring Bell`, `Ring Clav`, `Chaos Perc`, `Breath Pad` |
| Percussion | `Kick`, `Hi-Hat`, `Snare`, `Wind` |

The last column of each row is where the expanded voice earns its keep: the
plucked family is a **string model** rather than a filtered saw, the `FM` voices
are **phase modulation**, `CZ` is **phase distortion**, `Twin Registration` and
`Overdriven` are a **second oscillator** and **drive**, the morphs are the
**wavetable position**, and the notch and peak outputs are what make `Hollow Clav`
hollow and `Talking Lead` talk.

### The ensemble palette

Seventy-four ensembles ship with the build, in this order — the order the list
scrolls. **The order is a curation, not a taxonomy**: what is near the top is what
you are most likely to want, and moving an ensemble up or down changes no sound at
all. It is line order in `ensembles.toml` and nothing else, and the palette
fingerprint deliberately ignores it so that re-ordering never looks like a change
to the sounds.

The list is in four parts:

| Part | What it is |
| --- | --- |
| **The first screen** (21) | `Default` and then one ensemble per role, most-used first — a piano, an electric piano, a Wurli, an organ, a pad, a string, a pluck, a guitar, mallets, a bell, a clav, two basses, brass, a wind, a choir. Two showcase ensembles sit in here because they fill a role better than anything else does: `Wide Strings` and `FM Tines`. `Default` is first because it is the reference point the whole palette is described against, not because it is the best sound in it. |
| **Showcase, the usable half** (13) | Three different designs stacked, where the middle is one no other ensemble uses, and the sound is something you would reach for. |
| **Showcase, the edges** (17) | The same construction with the point being the *method* — cross-modulation, ring modulation, phase distortion, wavefolding, the drum kit. |
| **The original palette** (23) | Grouped so neighbours are the same family. |

**All 74 are three different designs** — a bass under, the named instrument in
the middle, a descant on top — because the first pass at this palette reused one
design in all three registers on everything outside the showcase, which is exactly
the thing an ensemble exists not to do. The middle is the ensemble's identity, so
no two ensembles share one. The drum kits are the only exception: a drum has no
register, so `Kick`, `Hi-Hat` and `Snare` are percussion in all three.

Within that, the two constructions are different on purpose. The original
palette answers "I want a Rhodes"; the showcase answers "what can this synth do",
and the answer is a different synthesis method in the middle of each one.
Judgements about which sound *good* are the player's, and the order is the one
place to record them.

#### The showcase

The middle column is what the ensemble *is* — a rule the palette's own guard
enforces across the whole file, because two ensembles sharing a middle instrument
are two names for one sound. Twenty-one ensembles carry an **insert chain** —
twenty-three slots between them, since `Folded Lead` and `Funk Clav` use two in
order — which is the palette's only use of the rack: before the showcase, no
shipped ensemble had one.

| Ensemble | Layers, low → mid → high | What it demonstrates |
| --- | --- | --- |
| `X-Mod Bell` | `Deep Sine` → `X-Mod Clang` → `Ring Bell` | Exponential cross-modulation at audio rate, ring-modulated on top |
| `Glass Cathedral` | `Through-Zero Bass` → `Glass low` → `FM Bell` | The glass table under a through-zero bass, with an FM bell above |
| `Growl Stack` | `Electric Bass` → `Growl Bass` → `Folded Saw` | A linear-FM growl through a tube stage, folded saw on top |
| `Through Zero` | `Sub Bass mid` → `Through-Zero Bass` → `X-Mod Clang` | Through-zero FM as the identity rather than a colour |
| `Ring Bells` | `Sub Bass mid` → `Ring Bell` → `Glass Bell` | The ring modulator at two frequencies, with a tremolo on the top |
| `Ring Clav` | `Electric Bass` → `Ring Clav` → `Hollow Clav` | The ring modulator on a clav, with the notch output above it |
| `CZ Lead` | `Folded Saw` → `CZ Lead` → `Talking Lead` | Phase distortion through a jet flanger |
| `CZ Organ` | `Gedeckt` → `CZ Organ` → `Twin Registration Organ` | Phase distortion in an organ, under a slow rotary |
| `Steel And Sitar` | `Upright Bass` → `Sitar` → `Steel Guitar` | A Karplus-Strong string through an auto-wah |
| `Electric Bass` | `Sub Bass mid` → `Electric Bass` → `Accent Clav` | The electric bass, compressed |
| `Harpsichord` | `Deep Sine` → `Harpsichord` → `Muted Guitar` | The plucked-string model at its brightest, bare |
| `Jazz Guitar` | `Upright Bass` → `Muted Guitar` → `Hollow Clav` | The muted-guitar model, bare |
| `Folded Lead` | `Electric Bass` → `Folded Saw` → `Buzz Lead` | A wavefolder into a resonant peak — two slots, in order |
| `Chaos Kit` | `Kick` → `Chaos Perc` → `Hi-Hat` | Feedback percussion through a bitcrusher |
| `Breath Choir` | `Choir Ooh` → `Breath Pad` → `Morph Choir` | Noise and a breathing filter under a morphing choir |
| `Morph Choir` | `Choir Aah` → `Morph Choir` → `Breath Pad` | The wavetable position swept, with a vibe on the middle |
| `Glass Morph` | `Glass low` → `Glass Morph Pad` → `Glass Bell` | The glass morph pad through a bandpass |
| `Twin Drawbars` | `Gedeckt` → `Twin Registration Organ` → `X-Mod Organ` | The second oscillator's registration, under a rotary |
| `Supersaw Lead` | `Folded Saw` → `Talking Lead` → `CZ Lead` | Four-voice unison through an ensemble chorus |
| `Wide Strings` | `Warm Pad` → `String Pad low` → `PWM Pad` | A string ensemble through the string-machine chorus |
| `Phase Pad` | `Warm Pad` → `Phase Pad` → `Glass Morph Pad` | A deep phaser on the pad's identity |
| `Celesta Bells` | `Sub Bass mid` → `Celesta` → `Glass Bell` | A struck celesta with a chorus, glass bells above |
| `Dirty Tine` | `FM Bass` → `Dirty Tine` → `Accent Clav` | The dirty tine with a dimension chorus |
| `Honky Tonk` | `Upright Bass` → `Honky Tonk Tine` → `Accent Clav` | The detuned tine, bare |
| `FM Tines` | `FM Bass` → `FM Tine Piano` → `FM Bell` | Phase modulation through a chorus |
| `FM Brass` | `Deep Sine` → `FM Brass` → `Trumpet` | Phase modulation with a trumpet above it |
| `FM Marimba` | `Sub Bass mid` → `FM Marimba` → `Celesta` | Phase modulation as a mallet |
| `Bell Choir` | `Deep Sine` → `FM Bell` → `Ring Bell` | An FM bell with a bell-tone ring modulator above |
| `FM Bass` | `Sub Bass mid` → `FM Bass` → `FM Bell` | Phase modulation at the bottom of the palette |
| `Overdriven Organ` | `Gedeckt` → `Overdriven Organ` → `Rock Organ` | The second oscillator driven, with a rock organ above |
| `Sine Pad` | `Sub Bass mid` → `Deep Sine` → `Glass Bell` | One computed sine, an ensemble chorus and a glass top |
| `Funk Clav` | `Electric Bass` → `Accent Clav` → `Ring Clav` | An auto-wah into an overdrive — two slots, in order |

#### The original palette

| Family | Ensembles | What makes them different from each other |
| --- | --- | --- |
| Starting points | `Default`, `Warm Pad`, `Plucky`, `Bassy`, `Glass` | The plain cases, kept as the reference points |
| Organs | `Drawbar Organ`, `Rock Organ`, `Pipe Organ`, `Gospel Organ`, `Gedeckt` | Tonewheel registrations — jazz, full, bright — under a rotary; the same into a resonant filter; a pipe diapason in a building; the jazz registration with the percussion tab down, so only the top register gets the ping; and a stopped flute, which is the odd-harmonic family and therefore accompanies rather than leads |
| Electric keyboards | `Electric Piano`, `Rhodes Bright`, `Rhodes Dark`, `Wurli`, `Wurli Bark`, `Clav`, `Grand Piano` | The Rhodes is one instrument with a tine control: stock, tine-forward, and tine dialled out. The Wurli is the soft reed and the one you dig into. Then the Clav, and a struck string with all its harmonics — the one thing the electric pianos cannot be, since they have a tine and this has a soundboard |
| Struck and plucked | `Bell`, `Marimba`, `Vibraphone`, `Kalimba`, `Harp`, `Nylon Guitar`, `Pizzicato` | A long decay into a long release; wood gone in half a second; the same idea with the motor running, which is the whole instrument; a thumb piano; a pluck left to ring; nylon, rolled off at the top; and the same string plucked by a section |
| Pads and voices | `String Pad`, `PWM Pad`, `Vox Pad`, `Choir Aah`, `Choir Ooh`, `Buzz Lead` | Three detuned saws with vibrato; a square whose duty cycle the LFO sweeps; and three formant timbres — the derived `/a/` and `/u/` vowels plus the earlier two-peak approximation |
| Winds and brass | `Soft Horn`, `Trumpet`, `Clarinet`, `Oboe`, `Flute` | The same reed spectrum at two brightnesses and two filter contours; then the odd-harmonic family, which is a cylindrical bore stopped at one end; the conical bore, which has all the harmonics climbing to a formant; and a flute, which has almost none of them plus audible breath |
| Basses | `Sub Bass`, `Acid Bass`, `Upright Bass` | A sine under a dark saw; the 303; and gut strings played with the side of the finger, with a little portamento because a hand does not teleport |
| Drums and effects | `Kick`, `Hi-Hat`, `Snare`, `Wind` | A click, a thump and a sub decaying at three rates; noise through a highpass; a low thump under bandpassed and highpassed noise; and a very slow filter contour drifting on a slow LFO |

### Roles, and which ones are covered

The chords this tool plays run from triads to 13ths, altered, sus, diminished and
augmented. The question worth asking of a palette is not "how many ensembles" but
"can I play the music these chords imply":

| Role | Covered by |
| --- | --- |
| Jazz comping | `Electric Piano` and the two Rhodes variants, `Wurli` / `Wurli Bark`, `Clav`, `Vibraphone`, `Drawbar Organ` / `Gospel Organ`, `Grand Piano` |
| Ballads and pads | `Warm Pad`, `String Pad`, `PWM Pad`, `Vox Pad`, `Choir Aah` / `Choir Ooh`, `Rhodes Dark`, `Glass Cathedral`, `Wide Strings`, `Phase Pad`, `Sine Pad`, `Breath Choir` |
| Sustained melody over chords | `Soft Horn`, `Trumpet`, `Clarinet`, `Oboe`, `Flute`, `Buzz Lead`, `CZ Lead`, `Supersaw Lead`, `Folded Lead` |
| Bass | `Sub Bass` for synth, `Upright Bass` for jazz, `Acid Bass` for lines, `Electric Bass`, `Through Zero`, `FM Bass` |
| Struck and rhythmic | `Marimba`, `Vibraphone`, `Kalimba`, `Plucky`, `Pizzicato`, `Nylon Guitar`, `Harp`, and from the showcase `Celesta Bells`, `Harpsichord`, `Jazz Guitar`, `Honky Tonk`, `Dirty Tine`, `FM Tines` / `FM Marimba` |
| Ceremonial and church | `Pipe Organ`, `Gedeckt`, `Bell`, `Bell Choir`, `Twin Drawbars`, `Overdriven Organ` |
| Percussion | `Kick`, `Hi-Hat`, `Snare`, `Chaos Kit` |
| Texture | `Wind`, `Glass`, `Warm Pad`, `Glass Morph`, `Morph Choir`, `Steel And Sitar` |

**Still missing, and worth knowing.** No **saxophone** — a conical reed with a
formant that moves, and the one wind instrument whose character is a *changing*
formant, which a stored cycle cannot do. No **electric guitar or amplifier**, and
no **steel-string acoustic** (the nylon table rolled off the top is as close as
the palette gets, and `Steel Guitar` is the string model rather than a recording
of one). No **solo bowed strings** — `String Pad` and `Wide Strings` are ensembles
and `Pizzicato` is plucked, so there is nothing for a single sustained violin or
cello line. No **hand percussion** beyond the kit. No **ethnic instruments** beyond
the string model — no koto, steel drum, accordion or harmonica. And no **sound
effects**, beyond `Wind`: no risers, sweeps or drones.

Loading an ensemble applies the whole design *and* its note length, so `Clav`,
`Marimba`, `Bell` and `Hi-Hat` arrive as quarter notes where `Drawbar Organ` and
`String Pad` arrive whole. Every shipped value is inside the range the Synth panel
can reach — a test asserts it, because a built-in the arrows would snap is a sound
that changes the first time it touches it. A second test renders all
seventy-four and checks that each is finite and audible, because nobody is going
to spot a silent one among seventy by eye. A third is a fingerprint of every
shipped register's composed channel **and its rack and mixer**, which is what makes
a change to a shipped sound a failing test rather than a listening session. It is
re-recorded deliberately — the palette's re-voicing moved it — and it sorts by
name before hashing, so re-ordering the palette does not. A fourth asserts that **no
shipped curve is anything but flat**, which is the same promise at the EQ layer:
the equaliser was added without changing a single sound that was already there. A
fifth asserts that every shipped ensemble layers **three different designs**, and
a sixth that **no two shipped ensembles share a middle instrument**, which is what
keeps the palette seventy-four sounds rather than seventy-four names. And a seventh
asserts the thing that connects all of it to the audio: **loading an ensemble
writes the whole voice**, every field of it, into every one of the three registers.
That one is load-bearing rather than decorative — see below.

#### Loading a register is all or nothing

`apply_voice` writes what a sound *is*; `apply_channel` writes that plus where it
sits — level, transpose, pan, sends, curve, rack. The second is written in terms of
the first rather than repeating it, and that is the fix for a bug worth recording:
the two used to be lists that had to be kept in step by hand, and when the second
oscillator, the drive, the wavetable position and the string model were added to a
voice they were added to the picker's list and not to the loader's.

The consequence was not subtle. Loading an ensemble left all three registers
holding the *default* drive, second oscillator, wavetable position and string
model, and picking an instrument from the library — which writes all thirty-seven
fields — was the only path that filled them in. So a register was half-loaded until
you touched it, and touching it made it come alive: the ensemble's identity arrived
only when you re-selected the instrument it was already set to. Fifty of the
seventy-four ensembles lost at least one field this way, across a hundred and four
register-slots, and the fields lost were exactly the ones that make the
interesting instruments interesting — `X-Mod Organ` and `X-Mod Clang` had no
cross-modulation, `Through-Zero Bass` no through-zero FM, `Ring Clav` no ring and
no drive, `Twin Registration Organ` a single registration, every plucked-string
voice default damping, every morph parked at position zero.

The symptom a player notices is not "a parameter is missing" but "the registers
bleed into each other", because the register you had just touched was audibly
different from the two you had not. The two tests that hold it shut are
`synth::tests::loading_a_channel_writes_the_whole_voice`, which asserts over the
whole shipped palette that what the engine holds after a load equals what the file
says, and `synth::tests::changing_one_registers_instrument_leaves_the_others_alone`,
which asserts that editing one register leaves the other two bit-identical.

Two things are shared between registers **on purpose**, and both can read as bleed
when you are listening for separation. A note is voiced as itself plus an octave
below and an octave above, so every chord is played through all three channel
strips at once and auditioning an instrument sounds it through three strips, not
one. And the reverb and delay are single aux buses: all three registers are summed
into one tank, and the metronome click is summed into the mid register after its
fader, so it feeds the reverb at the mid register's send level.

### The EQ curve palette

Twenty-five curves ship, in this order — the order the `preset` row cycles. They
are starting points, mostly broad on purpose: the job of a graphic equaliser here
is to seat a part in the mix, not to fix a recording.

| Group | Curves |
| --- | --- |
| Reset | `Flat` — the only curve that is bypassed; the `preset` row's origin |
| Tone | `Bass Boost`, `Sub Bass`, `Kick`, `Thin`, `Warm`, `Bright`, `Air`, `Sizzle` |
| Shapers | `De-Mud` (200–400 Hz), `De-Box` (300–600 Hz), `Presence` (2–5 kHz), `Scoop`, `Smiley`, `Club` |
| Filters | `Hi-Pass 40`, `Hi-Pass 80`, `Hi-Pass 120`, `Lo-Fi`, `Vinyl` |
| Band-limits | `Telephone`, `AM Radio`, `Speech`, `Vocal`, `Snare` |

A test holds that the names are distinct, that no two curves are the same, that
every gain is inside ±12 dB **and on a half decibel**, and that `Flat` is the only
curve that bypasses. The half-decibel rule is not decoration: it is what makes the
panel's arithmetic exact, and exact arithmetic is what makes a band taken back to
zero a true bypass rather than a filter that is very nearly doing nothing.

## 8. What this synth is

Three channels, not sixteen voices. `allocate()` splits whatever chord is playing
by pitch — lowest note to `low`, top note to `high`, everything between to `mid` —
so each channel is really *a register*, and a voice is what fills it. Anything under three notes is padded up: one note becomes
root − 12 / root / root + 12, two become bottom / top / top + 12, a triad becomes
root / third / fifth. So **the channel a note lands in depends on the voicing, not
on the voice**, and transposing one register an octave moves with the voicing. That
is the one structural limit left, and it is why true drawbar registrations cannot
be expressed here.

Everything else is the standard subtractive chain, per voice and per channel:

| Stage | What it does |
| --- | --- |
| **Oscillator** | Four computed shapes — `sine`, `saw`, `square`, `triangle` — plus `noise` and **eighteen stored wavetables**. The square has a **`pulse width`** (5–95 %), which is the hollow, reedy half of its range; it is built from two levels scaled by the width, so it carries no DC offset and a narrower pulse is genuinely quieter rather than louder. A separate **`noise level`** mixes white noise *alongside* whichever shape is selected, which is the difference between a hi-hat and a snare |
| **Wavetables** | A stored single cycle built from a harmonic recipe, read with interpolation — one lookup per sample, which is how a nine-partial additive timbre fits in a synth whose every voice is one oscillator. Eighteen recipes, taken from a tonewheel drawbar reference, the pipe-organ stop families, the odd-harmonic physics of a stopped pipe, and measured vowel formants. Up to sixteen partials, peak-normalised, no DC, and clean across the tool's own key range; a voice that also transposes up near the top of the keyboard will push the highest partial past Nyquist, and aliases less badly there than the naive saw always does. Costing *less* than the sine it replaces, measured: a table read is an index and an interpolation where `sine` is a libm call |
| **Amp envelope** | Linear ADSR in the timing, with an **`env curve`** that bends a segment without changing how long it takes or where it lands. At 100 % the segments leave their endpoints the way a capacitor does — the difference between a marimba and a fade |
| **Filter envelope** | Its own attack and decay, with no sustain, feeding a signed **`filter env`** amount of up to five octaves either way. It is a separate contour because it has to be: a horn wants the filter open over 600 ms while the note itself starts in 100 |
| **Key tracking** | Up to one octave of cutoff per octave of pitch, anchored at middle C. The pairing that makes a filter envelope sound like an instrument rather than an effect |
| **LFO** | One per voice, restarting with each note, at a global `lfo rate` and `lfo wave`. The *destinations* are per channel: **pitch** (up to a semitone), **cutoff** (up to four octaves), **amp** (full tremolo) and **pulse width**, so one channel can shimmer while another pulses and a third is swept hollow |
| **Filter** | The Chamberlin state-variable filter, all three outputs computed every sample and **`filter type`** choosing which is heard: `LP`, `HP`, `BP` |
| **Unison** | 1–4 voices per note, spread across **`detune`** (up to 50 cents each way) and level-normalised so widening a voice does not get louder. The divisor follows how the stack actually sums: `√n` when it is detuned and therefore adds incoherently, `n` when it is not and therefore adds coherently |
| **Glide** | Portamento between one note and the next, up to two seconds. The first note of a session never glides — there is nothing to glide from |

### The equaliser, and where it sits

The filter above is part of what a voice *is*, and it is per voice. The
equaliser is the opposite: it is per **part**, and it is applied on the mix bus
rather than inside any voice.

| Curve | Applied to | Where exactly |
| --- | --- | --- |
| one per placement | that register's bus, after every voice in it has been summed | before the register's fader, so the reverb send is shaped too |
| the master curve | the finished left and right pair | after the reverb, before the master gain and the soft clip |

Four coefficient sets driving five filters — the master's coefficients are
designed once and run twice — so 65 biquads a sample, measured at **1–1.5 % of
one core** with every band moved (`cargo test --release --bin chord-tool
the_equaliser -- --ignored --nocapture`). Per voice it would be thirteen biquads
times 121 voices, more than the entire rest of the callback, for a decision that
belongs to the part rather than to the note.

A curve that is flat in all thirteen bands is **not run at all**, exactly rather
than nearly: `tick` returns its input untouched and writes nothing to the filter
memory. That is what lets the equaliser be added to this crate without changing
any sound that was already here, and a test asserts that every shipped curve is
flat.

Changing a band redesigns the thirteen sections and leaves the running memory where
it was, which is what a hardware equaliser does: the transient is the size of the
signal already in flight. A band that is exactly 0 dB is an identity section and
skips its arithmetic, so a curve with one band moved runs one band's worth of work
and twelve copies.

The Bell and shelf recipes are the RBJ audio cookbook's, in direct form II
transposed, and two details are deliberate rather than incidental. The corner of
any band is clamped below Nyquist, because a bell whose corner sits *at* Nyquist
has `sin(w0) = 0` and degenerates into a pair of poles on the unit circle — a
resonator that grows instead of a filter that decays; the top band is the only one
that can get there, and only on a low-rate device.

The second is that **both end bands are shelves whose corners sit near the edges
of the audible range**, so neither reaches a flat plateau on a 44.1 kHz device:
+12 dB at the 16 kHz corner is about +6 dB there and merely less above it, and
+12 dB at the 20 Hz corner is about +6 dB at 20 Hz with the real lift below. Each
only ever touches its own end of the range, which is what it is for — the 20 Hz
band lifts the very bottom without thumping the 40–100 Hz that the 31.5 Hz bell
handles — but both read gentler than the number on the row suggests, and that is
geometry rather than a fault.

### The spectrum, and why it is a filter bank

`analyzer.rs` is thirteen bandpass biquads in parallel per tap, each with an
envelope follower. Four taps: the three registers and the mix.

| | |
| --- | --- |
| **Bands** | the EQ's own ladder, so a level and the curve shaping it share a column |
| **Filters** | `Section::bandpass` — the same biquad the EQ's bells use, constant peak gain so a full-scale sine reads full scale wherever it lands |
| **Q** | 2.0, chosen against the ladder: a tone a full octave off centre reads about 10 dB down, and one two thirds of an octave off — the nearest neighbour at the bottom — about 7 |
| **Ballistics** | instant attack, exponential release, 48 / 24 / 8 dB per second |
| **Cost** | 52 filters a sample, **about 1 % of one core**, measured (`cargo test --release --bin chord-tool the_analyser -- --ignored --nocapture`) |
| **Where read** | `low`/`mid`/`high` after that register's curve, rack and fader, before the pan; `master` after the master curve, the master gain and the soft clip |
| **Published** | linear levels, once per buffer, as 52 `SharedF32` values the panel polls each frame |

**Why not a transform.** An FFT would resolve far more than thirteen columns and
every bit of that extra resolution would be thrown away by the display; it needs a
dependency, a window function and a buffer of history; and its cost is a function
of its size rather than a fixed number of multiplies. A bank can be pointed at
*exactly* the ladder the equaliser uses, which is the property that makes the two
panels read as one tool — the same argument as a stored single cycle against a
transform in the oscillator.

Two details are deliberate. **The peak-gain form of the bandpass**, not the
constant-skirt one: an analyser that scales every band by its own bandwidth is
reporting something other than level. And **the filter's tail is floored to
silence**: a bandpass ringing down through the denormals produces values far below
audibility for ever, which costs real time on some hardware and would keep the
envelope from ever reaching zero. Both were caught by tests rather than by ear.

### Tonewheels, and what the wavetables fixed

An additive registration used to have nowhere to live here, because `allocate()`
gives each channel a *register* rather than a harmonic — so `Drawbar Organ` was
three sines standing in for nine drawbars. It is now nine drawbars.

The obstacle was that a wavetable is periodic and can only hold whole-number
multiples of its own fundamental, while the nine drawbars sit at 0.5, 1.5, 1, 2,
3, 4, 5, 6 and 8 times the played note — and two of those are not whole numbers.
Multiply the set by two and it becomes 1, 3, 2, 4, 6, 8, 10, 12 and 16. So a
drawbar table's fundamental is the **16′ drawbar**, one octave below the key you
pressed, and the eight-foot drawbar is its second harmonic. You play C4, the
table's fundamental is C3, and all nine drawbars land where a tonewheel organ
puts them. `Drawbar Organ` uses a different registration on each of the three
registers, which is one more than a real organ offers.

This is worth being precise about, because it is a **timbre** fix and not a
**voicing** one: it changes what one note is made of, and leaves `allocate()`
exactly as it was. Two things are still approximated:

- **One treatment per note.** The register split hands each note to exactly one
  channel, so two different treatments of the *same* note — a percussive pluck
  under a sustained pad — still have nowhere to live. That would need layer mode,
  which is a different feature with a real voice-pool cost.
- **Noise** is either the selected waveform (pure noise, per voice and therefore
  independent per note) or a level mixed alongside a tonal one, which covers
  hats, snare bodies, breath and wind but is not a separate envelope-contoured
  source.

---

## 9. The effect rack

Every register has a **chain of six insert slots**, in order, plus two **aux
sends**: a reverb send and a delay send, each feeding one master unit whose return
level is a mixer row.

An effect is three things: a **kind** (the family), a **subtype** (the variant
within it), and up to **six parameters**. The pair is what a slot stores; a slot
is empty (`none`) until you choose a kind, and an empty slot costs nothing,
because the audio callback checks each rack once per buffer and skips it whole.

### The fifteen kinds

Fourteen families, and the empty slot that is the absence of one:

| Kind | Variants |
| --- | --- |
| `none` | — |
| `reverb` | `hall`, `room`, `plate`, `chamber`, `ambience` |
| `delay` | `digital`, `tape`, `analog`, `slapback` |
| `chorus` | `chorus`, `ensemble`, `vibrato`, `dimension`, `rotary` |
| `flanger` | `flanger`, `jet`, `thru-zero` |
| `phaser` | `phaser`, `vibe`, `stepped` |
| `distortion` | `overdrive`, `soft`, `hard`, `tube`, `fold`, `rectify` |
| `fuzz` | `fuzz`, `germanium`, `gate`, `spit` |
| `bitcrusher` | `crush`, `decimate`, `radio` |
| `ringmod` | `ring`, `bell`, `am` |
| `tremolo` | `sine`, `square`, `ramp`, `chop` |
| `filter` | `lowpass`, `highpass`, `bandpass`, `notch`, `peak` |
| `wah` | `auto`, `pedal`, `lfo` |
| `compressor` | `comp`, `limiter`, `punch` |
| `gate` | `gate`, `stutter`, `duck` |

A variant is a **starting point**, not a label: choosing one loads that variant's
own defaults, because `plate` and `hall` are the same four slots with a different
size and damping, and a choice that did not move them would be inaudible. Fifty-four
variants ship across the fourteen families.

Each kind declares a prefix of the six parameter slots, so nothing is scattered:

| Kind | Parameters |
| --- | --- |
| `reverb` | `size`, `damp`, `predelay`, `mix` |
| `delay` | `time`, `feedback`, `tone`, `mix`, `sync`, `division` |
| `chorus` | `rate`, `depth`, `spread`, `mix` |
| `flanger` | `rate`, `depth`, `feedback`, `mix` |
| `phaser` | `rate`, `depth`, `feedback`, `mix` |
| `distortion` | `drive`, `tone`, `level`, `mix` |
| `fuzz` | `drive`, `bias`, `gate`, `level` |
| `bitcrusher` | `bits`, `rate`, `mix` |
| `ringmod` | `freq`, `depth`, `mix` |
| `tremolo` | `rate`, `depth`, `mix` |
| `filter` | `cutoff`, `resonance`, `drive`, `mix` |
| `wah` | `sens`, `range`, `resonance`, `rate`, `mix` |
| `compressor` | `threshold`, `ratio`, `attack`, `release`, `makeup` |
| `gate` | `threshold`, `attack`, `hold`, `release`, `rate` |

Only the ones the aux units use are on the master block: `reverb size`, `damp`
and `predelay`, and `delay time`, `feedback`, `tone`, `sync` and `division`. The
two `mix` slots are the ones the aux position never reads.

`sync` is the one that needed a sixth slot: a delay locked to a note value is
locked to the *tempo*, so the duration is the tempo's and the slot holds the
**division** instead. That is why switching `sync` on changes the sound even
though no millisecond value moved — the delay time becomes the note value, and it
follows every tempo change from then on.

### Where the rack sits in the signal flow

```
register bus ─▶ EQ ─▶ chain ─▶ fader ─▶ pan ─▶ dry ──┐
                                                     ├─▶ master EQ ─▶ gain ─▶ tanh
reverb send ─▶ reverb ─▶ × level ────────────────────┤
delay send  ─▶ delay  ─▶ × level ────────────────────┘
```

The chain is **pre-fader**: the EQ and the rack shape the sound, then the fader
decides how much of it there is. The sends are taken **after** the fader — sending
a register you have faded out to the reverb is not what a fader means — and
**before** the pan, so the send is one mono tap per register and panning a
register does not change how much of it reaches the reverb. The returns are
**additive**: reverb only ever adds, and turning it up never hollows out the dry
signal underneath it. Both aux units are fully wet; how much of them you hear is
the return level, which is a mixer row of its own.

Two consequences worth knowing:

- **A return at level 0 is not run at all.** Turning a send up from silence starts
  the unit from silence, so a reverb tail is not waiting there from a bar ago.
- **The aura of a chain is not undone by a mute.** Muting the master mutes
  everything, but a rack stays wired.

### The racks are per register, and that is tested

Three registers means three racks, and they are three disjoint runs of six
`FxState`s in one flat bank — eighteen slots in all, plus the two aux units. Each
state owns its own delay line, comb bank, allpasses, LFO, phaser stages,
one-poles, DC blocker and scalars, and a change of effect type resets *that slot
and no other*.

So a change to the high rack cannot move the low part, and this is not left to the
shape of the types. Four tests hold it: the bank's slots must stay disjoint, a
parameter change in one rack must not move another in the rendered mix, the FX
panel's column cursor must write the rack it names, and a curve on one register
must not move another (the equaliser is four separately named banks with four
separately named states, so it has no index to get wrong — but a copy-pasted state
in the sample loop would be just as silent).

Two things are shared on purpose, and both can read as bleed:

- **The reverb and delay are single buses.** All three registers are summed into
  one tank, so a driven high part does add to the reverb tail under the low end.
  The send is taken after the fader, so a register at zero volume reaches neither
  bus — muting a part takes it out of the reverb as well, which is a property and
  not an accident, and one of the tests asserts it.
- **The metronome click is summed into the mid register** after its fader, so it
  is panned and sent to the reverb as if it were the mid part. It bypasses the
  progression's mute and gain, because it is a rehearsal aid rather than music.

### What it costs, measured

The whole rack — eighteen inserts and both aux units, every slot filled, driven
with a real signal — is **about 2 % of one core** at 48 kHz
(`cargo test --release --bin chord-tool -- --ignored --nocapture`). It measures
1.7 % on an idle machine and 3.6 % on a busy one, which is the honest spread of a
benchmark run from inside the test suite.

That is the strategic fact about this feature: the racks are **bus-level** work,
done once per sample rather than once per voice. The voice pool, which *is*
per-voice, measures 11–29 % for the same material — so a fully loaded rack costs
less than an eighth of the pool it plays through, and the same distortion inside
every voice would be 121 copies of it.

All twenty `FxState`s are preallocated when the synth is built — about 8 MB, mostly
delay lines — and the audio callback never allocates. An empty slot is skipped in
the same loop it would have run in, and a rack with nothing in it is skipped
before its first slot is touched.

### What is deliberately not modelled

- **Convolution reverb.** The tank is four combs into two allpasses, scaled by
  `size` and damped by `damp` — the same algorithm this crate has always had. The
  five variants are voicings of it, not five rooms. A convolution engine would
  mean shipping impulse responses and a partitioned FFT.
- **Tape saturation with hysteresis.** `tape` delays are voiced, not modelled:
  the wow and flutter and the soft clip are there, the magnetic physics is not.
- **A real optical or FET compressor.** `compressor` is a feed-forward peak
  detector with a quadratic knee — no lookahead, no program dependence, no
  attack/release curve that changes with level. It is a leveller, not a bus
  compressor with a character.
- **Oversampling.** The distortions and the bitcrusher alias, deliberately: the
  aliasing *is* part of what a bitcrusher sounds like, and oversampling the
  distortions would double or quadruple their cost for a difference that is
  mostly below the noise floor of a chord.
- **True stereo.** Every register's chain and every send is one mono signal,
  panned at the very end, so a chorus widens a register but not the stereo image
  of the mix; the tank's output is summed to mono before it returns, and a
  stereo send would mean two chains' worth of state per slot.
- **Sidechain, per-slot sends, and a wet/dry per chain.** One send per register per
  unit, one return per unit. A ducking gate listens to its own input.

---

## 10. The voice, expanded

What one voice can do, and why each addition is worth its cost. Every control here
is **skipped at its neutral value**, which is what keeps the shipped palette
rendering to the sample: a patch that does not use a feature does not pay for it
and does not hear it.

### The filter has five outputs, not three

The state-variable form computes a lowpass, a highpass and a bandpass every
sample, and two more fall out of them for one add each:

| Output | What it is |
| --- | --- |
| `LP` / `HP` / `BP` | the three the filter has always had |
| `NT` — notch | `low + high`: everything *but* the band, a null at the corner |
| `PK` — peak | `low - high`: the band against the rest, unity gain at the corner |

A notch is a hollow, phasey colour no setting of the other three reaches, and a
peak is a bandpass that does not get louder as the resonance comes up — which is
what you want under a chord when `BP` would be a whistle.

### Drive, and what it does to the level

`drive` is a pre-gain of one to ten into a saturator, with **no make-up gain**: a
driven filter is louder as well as richer, which is what the knob does on the
hardware it imitates. The saturator is `x / (1 + |x|)` rather than `tanh` — one
divide against a libm call, and this runs once per voice rather than once on the
bus. At zero the stage is not run at all.

### Velocity, which is the accent

**Velocity is the rhythm's per-cell accent.** It already reaches the voice, as the
note's gain, because an accent is a change of loudness; what is new is that it
arrives a *second* time, as its own value, so a patch can make an accent a change
of tone as well:

| Row | What a soft hit does |
| --- | --- |
| `vel cutoff` | closes the filter, up to four octaves at full depth |
| `vel pwm` | widens the pulse, which is duller and thinner of harmonics |

Both are zero at full velocity, and both are skipped at zero depth, so a patch
with no accents in it is untouched. `vel level` is deliberately *not* a row: the
gain already is that, and a second control scaling the same loudness would be two
knobs for one job.

### The wavetable position blends two spectra

`position` fades from the chosen waveform into the next one **in its octave
group**, so `glass` becomes `vox` and `vox aah` becomes `vox ooh`. This is the one
thing a filter cannot do: a filter tilts the *envelope* of a spectrum, while a
position moves between two different sets of partials. A test asserts that
`position` at one is the partner waveform to the last bit, and another that half
way holds *both* spectra at once.

The group matters. Two tables an octave apart cannot be blended at one phase —
the drawbar registrations advance their phase at half the rate of everything else
— so the pairs never cross that line, and the four drawbar tables morph among
themselves. `pluck` is its own group: a string is not a spectrum, so the row
leaves a plucked voice alone.

### Phase distortion

A two-segment bend of the cycle: the breakpoint sits at the middle at amount 0 and
moves towards the start as the amount comes up, compressing the rising half of the
wave into a shorter and shorter span. A sine becomes a ramp, and its **period does
not move** — which is what tells it apart from a filter and from a pitch change.
Zero is exactly identity, not nearly: both halves are multiplied by
`0.5 / 0.5`, which is exactly one.

### The second oscillator

A whole second oscillator with its own waveform, interval (±24 semitones) and
level, plus a fourth control that is not a mixer: `osc2 fm` adds the second
oscillator to the *first* one's phase before the lookup.

| Row | What it does |
| --- | --- |
| `osc2 waveform` | twenty-three shapes — everything but `pluck`, because one voice has one string |
| `osc2 interval` | its pitch, in semitones from the first |
| `osc2 level` | how much of it is mixed in |
| `osc2 fm` | how hard it bends the first oscillator's phase |

Phase modulation rather than frequency modulation: the carrier's pitch never
moves and the sidebands stay symmetric, which is the difference between a bell and
a wobbly detune. At a level of zero *and* a depth of zero the second oscillator is
not started at all.

### Cross-modulation, which is not phase modulation

`osc2 fm` bends the first oscillator by the second one, and `fm mode` chooses
*which part* of it is bent. The three are genuinely different sounds, and the
difference is what happens to the index as the note moves:

| Mode | What is bent | The index | Sounds like |
| --- | --- | --- | --- |
| `phase` | the carrier's phase | a fixed number of cycles at every pitch | every DX-style patch: bells, tines, brass |
| `linear` | the carrier's frequency, in hertz | `deviation / modulator`, so it grows as the note falls | a growl: enormous under a bass, nearly gone at the top |
| `expo` | the carrier's frequency, by a ratio | the interval between the oscillators, so it is the same everywhere | the analog X-Mod: a clang that tracks the keyboard |

`linear` is measured: `Growl Bass` is 4.8 times brighter at C2 than at C6, and
`X-Mod Clang` is the same at both ends. A deviation wider than the carrier's own
frequency runs the phase *backwards* for part of every cycle — **through-zero
FM**, free here because the phase is an accumulator rather than a lookup index,
and `Through-Zero Bass` is the patch that lives there.

`expo` needs one correction to be usable. The mean of `2^(D sin)` is `I₀(D ln 2)`,
above one, so an exponential X-Mod plays **sharp** — up to a hundred and forty
cents at the top of the row, which in a chord is a wrong note rather than a
colour. The mean is divided back out, which keeps the asymmetry inside a cycle
(that is the sound, and it is why an analog X-Mod rises further than it falls)
and removes only its average. The compensation is the small-argument series for
the Bessel function, six terms, exact for a sine modulator.

**`feedback`** is the oscillator bending *its own* phase with its previous
sample: one sample of delay is enough to fold it. A sine becomes a ramp, which is
the cheapest saw in the synth, and past the fold a ramp becomes broadband, which
is a percussion and breath source rather than a fault. The top of the row is
meant to be chaotic.

**`osc2 ring`** multiplies the two oscillators together, which puts their sum and
their difference in the output — both of which move with the note. The effect
rack has a `ringmod` too, but its oscillator is a fixed hertz, so it rings at one
pitch whatever is played; this one tracks. A direct-current blocker is not
optional: a sine multiplied by itself is half direct current, and this filter has
unity gain at DC, so the offset would ride into the reverb and the limiter.

### The plucked string

`pluck` is a waveform, and the only one that is a **model** rather than a shape: a
delay line whose length is the note's period, a one-pole lowpass in the loop, and a
burst of noise to start it. That is Karplus-Strong, and it is why a plucked note
sounds like a string rather than like a filtered saw — its partials die at
different rates and sit a few cents off the harmonic series, and a feedback loop
gets both for nothing.

| Row | What it does |
| --- | --- |
| `pluck decay` | how long the string rings, in seconds to −60 dB, 50 ms to 8 s |
| `pluck damp` | how fast the upper partials die |
| `pluck burst` | how long it is excited, as a fraction of one period — a short burst is a hard pick |

Two details are load-bearing. The loop's loss is levied **once per pass**, not once
per sample, so the decay is the same number of seconds at every pitch; a test
would catch a per-sample loss, which makes the ring time proportional to the note.
And the damping filter delays the loop as well as damping it, so the delay is the
period *less* that delay — without the correction every note is flat, and flatter
the more damping there is. The whole line is preallocated with the voice pool and
is only ever touched by a `pluck` voice.
