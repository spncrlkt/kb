# chord-tool cheat sheet

One page, for keeping open beside the app. [REFERENCE.md](REFERENCE.md) is the
full reference — every feature and every key — and this is the part you forget
while playing.

Keys are given by their **keycap**: the label printed on the key you press. The
app reads physical positions rather than characters, so on a QWERTY keyboard the
key labelled `y` still recalls the chord even though it types `f` under
Programmer Dvorak. If a key does nothing at all, see *Getting unstuck* below.

---

## Three kinds of key

They never overlap, which is what makes two hands and a hotkey work at once.

- **Chord keys** — the ten home-row positions `a s d f` `j k l ;`. They sound, and
  they are what the chord grammar reads.
- **Hotkeys** — the row *below* the home row, plus `g`. They act, and they
  deliberately never join the held chord, so they stay usable mid-chord.
- **Panel keys** — `Tab`, the arrows, `Enter`, `Esc`, `Space`, and one modifier
  combination.

## Playing a chord

A chord is the **set** of keys held, not a sequence.

**Left hand — the degree.** Any shape not in this table resolves to nothing.

| Keycaps | Degree | | Keycaps | Degree |
| --- | --- | --- | --- | --- |
| `f` | I | | `d` | V |
| `a` | ii | | `s` | vi |
| `a` `s` | iii | | `a` `s` `d` | vii |
| `d` `f` | IV | | | |

**Right hand — the transformation.** No `h` gives the plain diatonic triad.

| With `h` held — stays in key | | Without `h` — absolute qualities | |
| --- | --- | --- | --- |
| (nothing) | `maj7` | `j` | `7` |
| `j` | `add9` | `k` | `7b9` |
| `k` | `sus4` | `l` | `9` |
| `l` | `6` | `;` | `m7b5` |
| `j` `k` | `maj9` | `j` `k` | `sus2` |
| `j` `l` | `13` | `l` `;` | `6/9` |
| `k` `l` | `7sus4` | `j` `l` | `dim7` |
| `j` `k` `l` | `13(9)` | `j` `;` | `7#9` |
| | | `k` `l` | `m9` |
| | | `k` `;` | `aug` |
| | | `j` `k` `l` | `maj7#11` |
| | | `k` `l` `;` | `7#11` |
| | | `j` `k` `;` | `mMaj7` |
| | | `j` `l` `;` | `11` |
| | | `j` `k` `l` `;` | `13` |

Adding `;` to an h-mode shape is invalid by design.

## Hotkeys

The row below the home row, plus `g`. **`Shift` selects the second action on a
key**, never a new key.

<!-- cheatsheet-hotkeys:begin -->
| Keycap | You type | What it does |
| --- | --- | --- |
| `` ` `` | `$` | tap one beat of the rhythm being recorded |
| `1` | `&` | metronome click on / off |
| `3` | `{` | tap the transport: play/pause, restart, seek to the middle |
| `4` | `}` | the same — the key next door, so a miss still taps |
| `g` | `i` | recall the chord under the cursor into the registers, and arm the in-place audition |
| `z` | `'` | lock the right register |
| `/` | `z` | lock the left register |
| `x` | `q` | copy the selection |
| `c` | `j` | paste the clipboard after the block, or into the gap before position 1 |
| `v` | `k` | delete the selection |
| `b` | `x` | undo the last progression edit |
| `b` + Shift | `X` | redo the last undone progression edit |
| `x` + Shift | `Q` | copy one rhythm per selected chord to the sinko clipboard |
| `c` + Shift | `J` | lay the sinko clipboard across the selection, in order and repeating |
| `y` | `f` | put the selected log row back in the registers |
| `u` | `g` | step back through the log, sounding each chord for 200 ms |
| `i` | `c` | step forward through the log the same way |
| `o` | `r` | sound the selected row while held, fading over 500 ms when let go |
| `p` | `l` | cycle the log: away, the exact history, the top list |
<!-- cheatsheet-hotkeys:end -->

`g` is the one hotkey on the home row; it is not a chord key. `3` and `4` are the
only two keys that do the same thing, so a fumbled transport tap still taps.

## Panel keys

| Key | What it does |
| --- | --- |
| `Tab` / `Shift+Tab` | next / previous panel |
| `↑` / `↓` | move the row cursor |
| `←` / `→` | adjust the selected value, pick the column, or pick a side of `[MIDI]` |
| `Shift+←` / `Shift+→` | the coarse step, or the value nudge on a two-dimensional panel |
| `Shift+↑` / `Shift+↓` | extend the chord selection (**Progression** only) |
| `Enter` | the panel's own action: commit the held chord, edit a value, run a menu item |
| `Ctrl+Enter` | always commit the held chord, appending it, wherever the cursor is |
| `Space` | latch both registers — press twice to clear |
| `Esc` | cancel an editor or prompt; otherwise stop the transport, and a second press within 500 ms quits |
| `Cmd+A` / `Ctrl+A` | select every chord (**Progression** only) |

`Shift` is the coarse step on: `bpm` (ten), the pattern list (five), the EQ
`preset` (five), `track key` (six), `offset` (four), the EQ `band` (four) and the
EQ `gain` (±3 dB). Everywhere else it is the plain step, because a toggle is a
toggle.

## The seven panels

`Tab` cycles in this order, and **the last four share one slot on screen** — you
never see the synth table and the equaliser at once.

| Panel | What it is | Its own keys |
| --- | --- | --- |
| **Progression** | the chord list and the selection every edit works on | `Enter` commits here; the block hotkeys above |
| **Transport** | tempo, loop, metronome, playing, track key, master volume, `[MIDI]` | `Enter` on `metronome` opens its panel, on `[MIDI]` opens the chooser |
| **Sinko** | the rhythm editor for the chord Progression has selected | see below |
| **Synth** | the three channels side by side, in seven pages | `PageDown` / `PageUp` change page |
| **Ensembles** | one row per ensemble; `Enter` loads it | `Enter` on `[Save As...]` writes to `ensembles.user.toml` |
| **EQ** | thirteen bands, one curve at a time | `↑`/`↓` walk `target`, `preset`, `band`, `gain` |
| **Spectrum** | the live level readout; a display, nothing editable | shares the EQ's `target` cursor |

On **Synth** and **EQ**, `←`/`→` pick the *column* (or target) and
`Shift+←`/`→` change the value — the opposite of every other panel.

**Sinko** rows, in order: `chord`, `pattern`, `offset`, `quant`, `swing`, `hits`,
`length`, `accent`, `hold`, `mute`, `smooth`, `record`, four take lines,
`[New Pattern]`, `[Save Pattern As...]`, `[Copy Sinko]`, `[Paste Sinko]`. On
`hits`, `Enter` toggles the cell under the cursor — **off clears that cell in
every take**.

**Metronome**, opened from the Transport: `click`, `sound`, `volume`,
`subdivision`, `swing`. While it is open it takes every key, including `Esc`,
which closes it rather than stopping the transport.

## Getting unstuck

- **A hotkey does nothing.** Check `ACTIVE_LAYOUT` in `src/keyboard.rs` matches
  your OS keyboard layout. The app maps a typed character back to a *physical*
  key, so a mismatch moves every hotkey one key across.
- **A second `Esc` quits**, so a stuck panel needs one press, not two.
- **The metronome panel swallows every key.** `Esc` closes it.
- **`Space` latches both registers**; press it twice to clear them, or use `z`
  and `/` to lock one.
- **The chord readout names what is held** — if it says nothing, the left-hand
  shape is not one of the seven, and nothing will sound.
- **`[MIDI]` unfolds sideways**: `Enter` opens it, `←`/`→` pick `[EXPORT]` or
  `import`, a second `Enter` runs the bracketed one, `Esc` or `↑`/`↓` folds it up.

## Testing and measurement

Not for playing, but worth knowing they exist.

| Variable | What it does |
| --- | --- |
| `CHORD_TOOL_TIMING` | turns on the named timing scopes; `debug.log` gets a `[TIME]` line a second with the audio callback's load, its worst buffer and how many buffers missed |
| `CHORD_TOOL_RENDER` | test-only: writes every offline render to a `.wav` so a changed fingerprint can be listened to |

```sh
cargo test          # the whole suite, no audio device needed
scripts/check.sh    # and clippy, and the tests in release
scripts/bench.sh    # measure the audio path (criterion)
```

`[TIME]` reads about one per cent with nothing playing. If it reads several per
cent at rest, something is running that should not be.
