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
- [ ] Every panel fits: `Tab` through all seven stops at 80×35 without a row
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
- [ ] `pattern` shows `[8/23]`, `Shift` jumps five, and `(edited)` appears once a
      cell is moved away from the library version.
- [ ] On `hits`, `Enter` toggles the cell under the cursor: **off clears it in
      every take**, on puts it in the newest.
- [ ] Recording: `Enter` on `record` arms it, the click runs, and each tap of
      `` ` `` sounds the chord and adds a take. The four layer lines draw, newest
      at full level and each older one quieter.
- [ ] `length` and `accent` apply to the hit under the cursor, not the whole
      pattern.
- [ ] `mute` silences the end of the pattern's bar and cuts anything ringing in.
- [ ] `[Copy Sinko]` then `[Paste Sinko]` over a multi-chord selection lays the
      rhythms out in order and repeats to fill; the paste row names what is
      waiting.
- [ ] `[New Pattern]` gives the selected chord a blank rhythm, audible at once.
- [ ] `[Save Pattern As...]` writes to `rhythms.user.toml` under a new name, and
      a fresh launch still has it.

## 6. History and Top

- [ ] `p` (`l`) cycles the log: away → exact history → top list.
- [ ] On the history view, `u` (`g`) and `i` (`c`) step back and forward through
      the log, sounding each chord for 200 ms.
- [ ] `o` (`r`) sounds the selected row while held and fades over 500 ms when let
      go.
- [ ] `y` (`f`) puts the selected row back in the registers.
- [ ] A chord only enters the log when it was actually audible — holding a chord
      with the loop running and nothing armed should not log it.

## 7. Synth panel **⚠**

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

## 8. Ensembles

- [ ] `Enter` loads an ensemble: all three registers change, and the instrument
      row is named for all three at once.
- [ ] A loaded ensemble's mixer travels with it, including note length and the
      master curve.
- [ ] `[Save As...]` writes to `ensembles.user.toml`; a user ensemble with a
      shipped name **wins**, and a fresh launch still has it.
- [ ] Loading an ensemble whose instrument does not resolve degrades rather than
      failing — you should get a neutral voice and an error line in `debug.log`,
      not a crash.

## 9. EQ

- [ ] `↑` / `↓` walk `target`, `preset`, `band`, `gain`, `[Save Curve As...]`.
- [ ] `target` cycles `low`, `mid`, `high`, `master`, and the title names which.
- [ ] `preset` applies the **whole curve** at once; `Shift` steps five.
- [ ] `band` moves the cursor, `gain` moves that band ±0.5 dB (±3 with `Shift`).
- [ ] `Enter` on `gain` returns that band to 0 dB.
- [ ] A curve at all zeros is inaudible — sweeping every band back to flat should
      sound identical to bypassing the panel entirely.
- [ ] `[Save Curve As...]` writes to `eq_presets.user.toml` and survives a
      restart.

## 10. Spectrum **⚠** (this one changed)

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

## 11. FX rack **⚠**

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

## 12. MIDI export and import

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

## 13. Settings persistence

- [ ] Change the key, the tempo and the master volume, wait a moment, quit.
- [ ] Relaunch: all three are as you left them, and the default ensemble's own
      master volume does not override your setting.
- [ ] Delete `settings.toml` and relaunch: the app starts on the defaults without
      complaining.
- [ ] Corrupt `settings.toml` (write garbage into it) and relaunch: it reports
      the problem in `debug.log` and uses the defaults rather than refusing to
      start.
- [ ] The progression is **not** restored — that is deliberate.

## 14. The new timing line **⚠**

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

## 15. Things most likely to be broken

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
      rest. (42 of them; the audio suite checks this renders, not that it sounds
      right.)
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
