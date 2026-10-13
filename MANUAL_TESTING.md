# Manual testing checklist

For a hands-on pass over this batch before it is committed. `cargo test` covers
the audio path and the pure logic; it does not cover the terminal, the audio
device, the panels' feel, or anything you can only see with your eyes.

Run with `cargo run --release` — the audio is smoother and the new timing line is
meaningful only with an optimiser.

```sh
cargo run --release          # and keep debug.log open in another window
tail -f debug.log            # optional, for the timing checks
```

Not every box will be interesting. The ones marked **⚠** are where the diff is
largest or the behaviour is newest, and are worth doing even on a short pass.

---

## 0. Before you start

The one thing that makes every subsequent step confusing if it is wrong.

- [ ] **The keyboard layout matches your OS.** `src/keyboard.rs` has
      `ACTIVE_LAYOUT = Layout::ProgrammerDvorak`. The app turns a typed character
      back into a *physical* key position using that table, so if your OS is on
      QWERTY every hotkey lands one key to the left of where this list says. Check
      by pressing the key labelled `g` with the transport stopped: on a matching
      layout it recalls the selected chord, and on a mismatched one it does
      nothing. If it does nothing, change `ACTIVE_LAYOUT` to `Layout::Qwerty` and
      rebuild before going further.
- [ ] `cargo test` is green and `cargo clippy --all-targets` is clean.
- [ ] There is an audio output device and the app starts without
      `no audio output device available`.

Throughout, keys are given by their **QWERTY keycap** — the label on the key you
press — the same convention `REFERENCE.md` §1 uses. `CHEATSHEET.md` has the same
list on one page, if you would rather have it open beside the app.

---

## 1. Start-up and shutdown **⚠**

- [ ] First launch creates `settings.toml` only once you change something, and
      creates the five `*.user.toml` files empty.
- [ ] `debug.log` is truncated and rewritten on every launch.
- [ ] The default ensemble loads and the instrument row names all three registers.
- [ ] `Esc` stops the transport; a second `Esc` within 500 ms quits.
- [ ] Shrinking the terminal below 80×16 replaces the frame with a banner saying
      what is missing, rather than wrapping into a mess.
- [ ] Every panel fits: `Tab` through all nine stops at 80×35 without a row
      being clipped or the layout jumping.

## 2. Chord grammar and registers

- [ ] A left-hand shape alone gives the plain diatonic triad: `f` → I, `d` → V,
      `s` → vi, `a` `s` `d` → vii.
- [ ] `h` plus a right-hand shape gives the h-mode additions; with no `h` the
      j-mode qualities. `REFERENCE.md` §1 has the two tables — spot-check one
      from each.
- [ ] A left-hand shape that is not in the table sounds **nothing**.
- [ ] `Space` latches both registers; a second `Space` clears them.
- [ ] `z` locks the right register and `/` locks the left, and a locked register
      survives a new chord.
- [ ] The chord readout names what is held, and matches what actually sounds.

## 3. Transport and metronome

- [ ] `3` taps the transport: play/pause, restart, seek to the middle. `4` does
      the same.
- [ ] `bpm` moves one at a time, ten with `Shift`, and clamps at 40 and 240.
- [ ] `track key` walks all 24 keys and wraps.
- [ ] `master volume` on the Transport and on the Synth panel are the **same
      number** — change it in one place and the other follows.
- [ ] `Enter` on `metronome` opens the panel; it takes every key while open, and
      `Esc` closes it *rather than stopping the transport*.
- [ ] `1` toggles the click from any panel.
- [ ] Metronome `subdivision` and `swing` change the click as described.
- [ ] The click is only audible while a rhythm is being recorded or the metronome
      is on — it should not leak into normal playing.

## 4. Progression editing **⚠**

- [ ] `Ctrl+Enter` appends the held chord wherever the cursor is; plain `Enter`
      commits at the cursor.
- [ ] The cursor moves with `↑`/`↓`; `Shift+↑`/`↓` extends a selection, and the
      header reports `N selected`.
- [ ] `x` copies, `c` pastes **after the block**, `v` deletes, `b` undoes,
      `Shift+b` redoes. The header's `undo: yes` / `redo: yes` tracks it.
- [ ] **`n` replaces the slot you are on with the chord you are holding**, keeping
      that slot's rhythm and offset. It must work with **one** chord selected,
      where the menu is away — that is the case the menu cannot serve. Undoable
      like every other edit, and it flashes rather than guessing when nothing is
      held or nothing is selected.
- [ ] Pasting into the gap before position 1 puts the chord at the front rather
      than refusing.
- [ ] `Cmd+A` / `Ctrl+A` selects everything.
- [ ] `g` recalls the selected chord into the registers and arms the in-place
      audition — the audition sounds *in context* on the next pass, and the
      `playing` row shows `*` while it is armed.
- [ ] A chord sounds in red while the loop is on it.

## 5. Rhythm — the Sinko panel **⚠**

- [ ] The panel always describes the chord the **Progression** panel has
      selected; changing the selection there changes this panel.
- [ ] `quant` offers 2, 4, 8, **12**, 16, **24**, 32, 64, and a hold survives a
      grid change.
- [ ] `pattern` shows `[8/19]`, `Shift` jumps five, and `(edited)` appears once a
      cell is moved away from the library version.
- [ ] Assigning a **four-bar** pattern spreads its bars: the first lands on the
      selected chord and the other three on the chords after it, and a single
      `Ctrl+Z` takes all four back.
- [ ] On `hits`, `Enter` toggles the cell under the cursor — nothing else. The
      grid draws under the row, on the bar the row above is showing.
- [ ] `length` and `accent` apply to the hit under the cursor, not the whole
      pattern.
- [ ] Changing `quant` **re-quantizes** the existing hits onto the new grid; it
      does not throw them away.
- [ ] `mute` silences the end of the pattern's bar and cuts anything ringing in.
- [ ] `[Copy Sinko]` then `[Paste Sinko]` over a multi-chord selection lays the
      rhythms out in order and repeats to fill; the paste row names what is
      waiting.
- [ ] `[New Pattern]` gives the selected chord a blank rhythm, audible at once.
- [ ] `[Save Pattern As...]` writes to `rhythms.user.toml` under a new name, and
      a fresh launch still has it.

## 6. The four-bar Loops panel **⚠** — brand new

Reached with `Tab` until the title reads `Synth [loops]`, or by pressing `Enter`
on the Sinko panel's `loop` row. It borrows the Synth slot, so the panels below
it do not move.

- [ ] The panel lists `source`, `chord`, `bar`, `quant`, `hits`, `length`,
      `accent`, `hold`, `[Clear Bar]`, `[Record]` and `[Save Loop As...]`.
- [ ] **All four bars are drawn**, one grid per line under `hits`, numbered 1–4,
      with the bar the cursor is on marked `▸` and the bar the clock is on
      carrying the playhead (`X`/`+`). `←`/`→` on `bar` moves the marker.
- [ ] The grid reads `x` for a hit, `·` for a rest and `=` for a cell a note is
      still sounding through, so a held note is a line and not a dot.
- [ ] `source` offers `one chord, four times` and `the first four chords`; the
      `chord` row is only meaningful for the first.
- [ ] `Enter` on `[Record]` with nothing loaded **loads the four bars and starts
      looping them with the click**. The grid follows the clock down the four
      lines, wrapping back to 1.
- [ ] Each press of `` ` `` sounds the chord of the bar the clock is on — change
      `bar` by hand and the tap should still follow the *clock*, not the line you
      are looking at.
- [ ] Let it come round: the hits you placed play back, each with its own bar's
      chord. Four bars, four different chords, four different sets of hits.
- [ ] A tap that lands slightly late still lands in the bar it was played in.
- [ ] **Holding `` ` `` records a length.** Hold it for about a beat and let go:
      the `length` row should show roughly a beat on that hit, the grid should
      show only *one* hit (the cells it was held through are cleared), and the
      note should sound for that long when the loop comes round.
- [ ] **Holding the key is one note, not a burst.** Hold it for two or three
      seconds: the note should keep sounding and the bar should gain exactly one
      hit, not one per key repeat.
- [ ] **Nothing hangs.** After a tap, the chord must stop when the key comes up.
      The one to look for: record a few bars, tap once and *stop* — the last
      chord should go quiet. Then `Esc` mid-hold, and press `` ` `` and unload the
      loop with `[Record]` while the key is down: no chord may be left ringing in
      any of those.
- [ ] A note held over a bar line keeps sounding into the next bar, and the grid
      line it started on carries its length.
- [ ] `[Clear Bar]` empties the bar the cursor is on and leaves the other three
      alone.
- [ ] `Enter` on `[Record]` a second time **stops the recording but keeps the
      loop** — the hits keep playing and nothing is lost.
- [ ] `Enter` a third time unloads the bed and hands the progression back.
- [ ] `Esc` during recording stops the transport and the recording **without
      discarding the four bars**; pressing `Enter` on `[Record]` again brings
      them back.
- [ ] `[Save Loop As...]` asks for a name and writes all four bars to the
      palette as **one** pattern; it then appears in Sinko's `pattern` row and in
      `rhythms.user.toml`, and survives a relaunch.
- [ ] `Tab` walks on from here and comes back; nothing below the panel jumps.

## 7. History and Top

- [ ] `p` (`l`) cycles the log: away → exact history → top list.
- [ ] On the history view, `u` (`g`) and `i` (`c`) step back and forward through
      the log, sounding each chord for 200 ms.
- [ ] `o` (`r`) sounds the selected row while held and fades over 500 ms when let
      go.
- [ ] `y` (`f`) puts the selected row back in the registers.
- [ ] A chord only enters the log when it was actually audible — holding a chord
      with the loop running and nothing armed should not log it.

## 8. Synth panel **⚠**

- [ ] `PageDown` / `PageUp` walk the seven pages and wrap; the title shows
      `Synth [filter 5/7]`.
- [ ] Paging does not move the panels below it — every page is padded to the same
      height.
- [ ] `←` / `→` pick a column (`low`, `mid`, `high`); `Shift+←`/`→` change the
      value.
- [ ] `tone` page: every waveform is selectable, and `position` audibly morphs a
      wavetable; `phase dist` changes the shape without moving the pitch.
- [ ] `osc` page: `osc2 level`, `osc2 fm` and `osc2 ring` each make a sound, and
      `fm mode` changes its character. Load **X-Mod Clang** or **Ring Bell** from
      the Ensembles panel to hear them set up.
- [ ] `pluck` page: **Plucky** or a nylon instrument rings and decays.
- [ ] `filter` page: `resonance` up with `cutoff` down rings without ever going
      silent or producing a click.
- [ ] `mod` page: `unison` 4 with `detune` thickens without getting louder; each
      LFO destination does something audible.
- [ ] `fx` page: all six slot rows show what that register's rack holds, and
      `Shift+←`/`→` on a slot row swaps the whole family.
- [ ] **Turning any knob while a chord is held does not click, and the change
      arrives within a buffer or two** — the engine now reads coefficients once
      per buffer rather than per sample, which is a deliberate trade. This is the
      one behaviour in the batch that a player could conceivably feel, so it is
      worth doing deliberately: hold a chord and sweep `cutoff` slowly, then
      quickly.

## 9. Ensembles

- [ ] `Enter` loads an ensemble: all three registers change, and the instrument
      row is named for all three at once.
- [ ] A loaded ensemble's mixer travels with it, including note length and the
      master curve.
- [ ] `[Save As...]` writes to `ensembles.user.toml`; a user ensemble with a
      shipped name **wins**, and a fresh launch still has it.
- [ ] **The list scrolls.** There are seventy-four shipped ensembles and the panel
      shows twenty, so arrowing to the bottom must roll the window rather than
      running off the screen — the panel's height must not change as it slides,
      and `[Save As...]` is the last row of the same list, reachable at the end.
      Tab away from the panel and back: the window returns to the row you left
      the cursor on.
- [ ] **The first screen is the everyday palette, and you can move it.** The list
      opens on `Default` and then twenty one-per-role instruments — piano,
      electric piano, Wurli, organ, pad, strings, pluck, guitar, mallets, bell,
      clav, two basses, brass, flute, choir. Re-ordering is line order in
      `ensembles.toml` and nothing else; no test has an opinion about it, so move
      anything you like up or down and re-run the suite to confirm nothing else
      noticed. **Which of the showcase sounds you would actually reach for is the
      one judgement left open** — promote the ones you like into the first
      screen.
- [ ] **Every ensemble is three different instruments, and no two share a
      middle.** Step the `instrument` row through the whole list: each of the
      three columns should name a *different* design, and the **middle** one
      should never be the middle of any other ensemble — the middle is what the
      ensemble is, so two ensembles sharing one are two names for one sound. The
      three drum kits (`Kick`, `Hi-Hat`, `Snare`) are the one exception: all three
      of their registers are percussion, because a drum has no register.
      Twenty-one ensembles route a register through the rack, twenty-three slots
      between them — `Folded Lead` and `Funk Clav` use two in order — and the rest
      have no rack at all.
- [ ] **The palette stays level.** Click through the whole list with a chord held:
      no ensemble should need the master moved. A test asserts the spread across
      all seventy-four is under 21 dB, and it is 19.2 dB.
- [ ] **The three registers are three parts.** Play a triad and listen for a
      **bass**, a **chord** and a **descant** — not one sound with three layers.
      The root should be an octave below the chord, the chord itself in the
      middle, and the top note an octave above. Before the fix a triad came out as
      three notes inside a major third, which is the "same instrument played three
      ways" this was reported as. A ninth should sound all five tones: the root an
      octave down, the third, fifth and seventh in the middle and the ninth an
      octave up. Percussion is the exception — a drum has no register.
- [ ] **Load an ensemble, then re-pick the instrument it already has.** This is the
      test for the register bug, and it needs no reference recording: load
      `Twin Drawbars`, hold a chord, then on the `instrument` row step the *high*
      column one place and back to where it started. **Nothing should change when
      it comes back.** Before the fix, re-picking a register made it come alive —
      it gained the drive, the second oscillator, the wavetable position and the
      string model that the ensemble load had dropped — so stepping away and back
      audibly changed the sound and shifted the mix against the other two
      registers. Do it on all three columns, and on `X-Mod Organ` / `Through-Zero
      Bass` / `Ring Clav`, where the difference was the whole synthesis method.
- [ ] **The showcase instruments do what their names say**, now that they can:
      `X-Mod Organ` and `X-Mod Clang` cross-modulate, `Through-Zero Bass` is
      through-zero FM, `Ring Clav` rings and drives, `Twin Drawbars` really is two
      registrations, the plucked strings have their own damping, and the morphs
      move. If one of these sounds like a plain organ or a plain saw, the voice is
      not reaching the engine again.
- [ ] **Two shares are deliberate**, so do not report them: a note is voiced as
      itself plus an octave below and an octave above, so every chord and every
      audition plays through all three channel strips at once; and the reverb and
      delay are single aux buses that all three registers are summed into.
- [ ] **The racks are separate.** On the `fx` page of the Synth panel (or the FX
      panel itself), give the **high** register a loud, obvious chain — a
      distortion, a flanger, a bitcrusher — and listen: the low and mid registers
      must not change at all. Then sweep the `rack` row between `low`, `mid` and
      `high` and confirm the title and the slot contents follow the column you are
      on. Before you conclude anything, do the same with the high register's
      *volume* at zero: the rack still runs, and the low and mid must still not
      change.
- [ ] **A curve is per register too.** On the EQ panel, put +12 dB on `high` and
      then on `low`, with only the mid register audible: the mid must not move.
      Then move the mid's own curve and confirm it does — that is what proves the
      first check was measuring anything.
- [ ] Known gap: many rows name instruments that have no ensemble of their own
      (`X-Mod Clang`, `Sitar`, `Harpsichord`, `Electric Bass`, `Celesta`). That is
      the point — the ensemble is the only way to reach them — so to hear one on
      its own, load its ensemble and solo the register in the Synth panel rather
      than looking for it in the list.
- [ ] Loading an ensemble whose instrument does not resolve degrades rather than
      failing — you should get a neutral voice and an error line in `debug.log`,
      not a crash.

## 10. EQ

- [ ] `↑` / `↓` walk `target`, `preset`, `band`, `gain`, `[Save Curve As...]`.
- [ ] `target` cycles `low`, `mid`, `high`, `master`, and the title names which.
- [ ] `preset` applies the **whole curve** at once; `Shift` steps five.
- [ ] `band` moves the cursor, `gain` moves that band ±0.5 dB (±3 with `Shift`).
- [ ] `Enter` on `gain` returns that band to 0 dB.
- [ ] A curve at all zeros is inaudible — sweeping every band back to flat should
      sound identical to bypassing the panel entirely.
- [ ] `[Save Curve As...]` writes to `eq_presets.user.toml` and survives a
      restart.

## 11. Spectrum **⚠** (this one changed)

- [ ] The panel draws thirteen levels for whichever `target` the EQ panel set,
      and the two panels share that cursor.
- [ ] `range` and `speed` change how it is drawn; `hold` toggles the peaks and
      `[Reset Peaks]` drops them.
- [ ] **Tab away from the panel and back.** It should clear and start drawing the
      sound that is playing now, not resume a stale reading. The filters stop
      running while it is off screen — that is deliberate, and §14 is how you can
      see it.
- [ ] Play a chord, tab away for a few seconds, tab back: the bars should reflect
      the present, not the past.

## 12. FX rack **⚠**

- [ ] `rack` cycles `low`, `mid`, `high`; `slot` cycles six places; changing
      either resets the `param` cursor.
- [ ] `type` cycles the fifteen kinds and loads that kind's own defaults — it
      should sound like a *new* effect, not a renamed one.
- [ ] `subtype` does the same within a kind.
- [ ] `preset` walks the library for the kind and variant on screen and reads
      `custom` as soon as a parameter moves.
- [ ] Each kind makes a sound, on a held chord: reverb, delay (and its sync
      divisions), chorus, flanger, phaser, distortion, fuzz, bitcrusher, ring
      mod, tremolo, filter, wah, compressor, gate.
- [ ] `[Move Earlier]` / `[Move Later]` swap the slot with its neighbour **and
      the cursor follows the effect**; the ends refuse rather than wrapping.
- [ ] `[Empty This Slot]` sets it to `none` and the sound is unchanged from
      having nothing there.
- [ ] `[Save Effect As...]` writes to `fx_presets.user.toml` and survives a
      restart.
- [ ] **Leave a delay and a reverb tail running and wait.** Both should decay
      away to silence rather than ringing for ever — the effects now flush
      anything that decays below the noise floor, and that was a fix, not a
      behaviour you should be able to hear.

## 13. MIDI export and import

- [ ] `Enter` on `[MIDI]` opens the chooser sideways; `←`/`→` pick a side; a
      second `Enter` runs the bracketed one.
- [ ] `[EXPORT]` writes to `progressions/` with a dated filename, and the outcome
      sentence appears in the transport row and in `debug.log` in full.
- [ ] Importing that file restores the progression **with its rhythms**, so the
      Sinko panel shows the grids it was exported with.
- [ ] An empty progression exports nothing and says so.
- [ ] A relative filename on import resolves against `progressions/`; an absolute
      path is honoured.
- [ ] The exported `.mid` opens in another program and plays the same notes at
      the same tempo.

## 14. Settings persistence

- [ ] Change the key, the tempo and the master volume, wait a moment, quit.
- [ ] Relaunch: all three are as you left them, and the default ensemble's own
      master volume does not override your setting.
- [ ] Delete `settings.toml` and relaunch: the app starts on the defaults without
      complaining.
- [ ] Corrupt `settings.toml` (write garbage into it) and relaunch: it reports
      the problem in `debug.log` and uses the defaults rather than refusing to
      start.
- [ ] The progression is **not** restored — that is deliberate.

## 15. The new timing line **⚠**

The instrumentation added in this batch, and the one place a performance
regression is visible without a profiler.

- [ ] With nothing playing, `debug.log` gets one `[TIME]` line a second:
      `callbacks N, load X% last / Y% mean / Z% peak, 0 over deadline`.
- [ ] **At rest, the load should be about one per cent.** The silent floor was
      610 ns a frame and is now 147, so a quiet buffer should read well under
      1 %. If it reads several per cent with nothing playing, something is
      running that should not be — say so, because that is exactly the number
      this batch was about.
- [ ] Hold a big chord with the transport running: the load should rise into the
      low single digits, and `over deadline` should stay at 0.
- [ ] `CHORD_TOOL_TIMING=1 cargo run --release` adds the named scopes to the same
      line: `scheduler.bar`, `tui.frame`, import, export, start-up.
- [ ] No audio drops, clicks or glitches while the line is being written —
      the counter costs two `Instant`s and four relaxed stores a buffer.
- [ ] If you can, watch the load while **tabbing away from the Spectrum panel**.
      It should fall slightly; that is 52 filters a sample that are no longer
      being run.

## 16. Things most likely to be broken

The places where a mistake would hide from the automated tests.

- [ ] **The panels at unusual sizes.** 80×16 (the minimum), 80×35 (everything
      drawn), and a very wide terminal (the bar grid takes spare columns up to
      120). Check for a row clipped, a column misaligned, or a panel that jumps
      when you page.
- [ ] **A chord held across a panel change.** Holding a chord and tabbing should
      not cut it, and releasing should release it.
- [ ] **Chords at the extremes.** The lowest and highest track keys, with the
      widest voicing you can hold — nothing should wrap, go silent, or click.
- [ ] **Loading every shipped ensemble in turn** with a chord held. None should
      be silent, none should blow up, and none should be wildly louder than the
      rest. (All seventy-four; the audio suite checks this renders, not that it
      sounds right.)
- [ ] **The looper's four bars against the clock.** Record four bars with a chord
      on each, then count the passes: the hits must not creep early or late
      against the click, and the bar the grid is showing must follow the one you
      hear. Leave it looping for a couple of minutes and check the `bar` row still
      tracks the audio rather than drifting.
- [ ] **Starting another app that grabs the audio device** while playing. The app
      should survive the stream error rather than dying silently.
- [ ] **A very long session** — leave it running for ten minutes with the loop
      going. The timing line's `mean` should stay flat rather than creeping up.
- [ ] **Saving, then editing, then saving again.** A user instrument, ensemble,
      curve, effect and pattern, each saved twice under the same name: the second
      save should replace rather than duplicate.

---

## What to send back

For anything that fails, the useful things are: what you pressed, what you
expected, what happened, and — if it is audio — the relevant `[IN]`/`[TIME]` lines
out of `debug.log`. If a sound is wrong rather than broken,
`CHORD_TOOL_RENDER=/tmp/out.wav cargo test --test render` writes what the engine
actually produces, which is usually faster than describing it.

Two specific questions worth answering, because they are judgement calls rather
than bugs:

1. **Is a buffer of lag on the knobs acceptable?** Coefficients are read once per
   buffer now rather than per sample. §7 is the test; if a fast `cutoff` sweep
   feels stepped, that is the trade showing and it is worth saying so.
2. **Is the Spectrum behaviour right?** It no longer runs while the panel is off
   screen, and it clears when you come back. That is a deliberate trade for half
   a per cent of a core.
