# TODO

Short-term next steps for `chord-tool`, in priority order.

**Keyboard layout is fixed to Programmer Dvorak for this version.** It is not
configurable and that is deliberate — no runtime layout switching. (The
on-screen labels showing QWERTY names is a known cosmetic rough edge, not a
scheduling item.)

## 0a. Done: rhythm patterns (sinko) and the synth panel

- [x] **A refused key press fades away.** The bug: `nothing copied yet` sat in red
      on the Sinko panel for the rest of the session, because the status type had
      only two outcomes and *every* non-success was made to persist. That is right
      for an error with a reason to read (an export that could not write) and wrong
      for feedback about a keystroke, so there is now a third outcome, `Refused`:
      same red, but it holds for five seconds and fades like a confirmation. Which
      messages are refusals is a decision per call site — "select a chord first",
      "nothing tapped yet", "nothing to copy", "nothing to copy yet", "nothing to
      export", "no file name" — while real I/O errors still stay put.

- [x] **Focus emphasis, in colour and in weight.** The focused panel wears a
      bold band and a heavy `━━` rule while the others go dim; its selected row
      gets a grey band; the progression's rows keep yellow (cursor) and red
      (sounding); the chord being played, the keys a register holds and the grid's
      playhead are coloured. Every cue is doubled — rule weight plus colour, band
      plus bold — because `NO_COLOR`, a monochrome terminal or a colour-blind eye
      should not lose the answer to "where am I". **Found and fixed on the way:**
      `NO_COLOR=1` was set in this shell, and crossterm honours it by emitting bare
      resets, so the app had been drawing *no* colour at all here. `run_interactive`
      now calls `force_color_output(true)` — colour is state in a TUI, not
      decoration — and the tests force it too, so a styling assertion cannot depend
      on the developer's shell.
- [x] **`Tab` follows the layout.** The focus cycle is the order the panels are
      drawn in — chord list, transport, sinko, synth (and the synth's presets page)
      — so every press moves the cursor the way the eye reads, and `Shift+Tab` is
      its inverse. The app still opens on the transport, which is a rotation of the
      same cycle rather than a different order.
- [x] **Progression left, Transport right** (they were the other way round). The
      clipping rule had to flip with them: the left column is now the chord list,
      where a shortened row means a lost chord or rhythm name, so the *right*
      column is the one that yields — the transport's long lines are action
      statuses, which `debug.log` keeps in full — with a floor of 24 columns so
      the transport never disappears.
- [x] **Transport and Progression share one block**, transport left, chord list
      right: the two are read together and are short, so the pair costs `max` rows
      instead of the sum. Panels are rendered into buffers and stitched line by
      line, with the left column sized to its own content and measured by *visible*
      width (colour codes have none), clipped with `…` if it would push the chord
      list past the terminal. Default view 21 → 16 rows, Sinko 38 → 33.
- [x] **Panel order follows `Tab`.** Top to bottom: registers, Transport,
      Progression, Sinko, Synth — the same order the focus cycle walks, so `Tab`
      moves down the screen instead of up it, and
      `the_panels_are_drawn_in_the_order_tab_walks_them` keeps the two lists
      from drifting apart.
- [x] **Dense layout.** The header is three rows: title with the layout and the
      track key, the hands, then both registers side by side with padded cells
      (columns never jitter) followed by the chord readout. Each panel also lost
      the blank line it opened with — the `── name ──` rules separate them — and
      a modal is the one thing that still gets a blank before it. Net effect:
      default view 35 → 21 rows, Sinko 52 → 38, Synth 49 → 35.
- [x] **The screen carries no instructions.** Everything that explained a key
      was removed — the `Lock: ... -> right register` sentence, the `Layout:`
      line, the `Esc stop / Tab cycle / Log: debug.log` footer, `(tab to switch)`
      on every panel header, the Synth's `(tab out · ←/→ column · ...)` note, the
      Sinko recording note, `enter toggles` on the hits row, and `(empty — press
      enter to add chords)`. The keys and registers are now a five-row table of
      data: hands, one row per register with what it means, and the chord. The
      focused panel is marked by a cyan header instead of a note. Nine rows came
      out of every view (default 35 → 27, Sinko 52 → 44), and the README is the
      only key reference. Modal prompts keep their own `[Enter]`/`[Esc]`
      affordances, because there they are the control rather than a reminder.

- [x] **`{` and `}` both tap the transport.** `}` is the key next door on
      Programmer Dvorak; a one-key miss at a performance control used to do
      nothing at all. Two physical positions (`TopRow3`, `TopRow4`) carry the
      same hotkey rather than one position matching two characters, so the
      displayed keycap stays honest and neither becomes a chord key.

- [x] **A rhythm belongs to the chord, not to the library.** The bug: two chords
      both given `Quarters` shared one library entry, so editing the hits on one
      changed both. `ProgressionEntry.pattern` is now `Option<RhythmPattern>` —
      it *owns* a copy — assigning clones out of the palette, and
      `arrangement()` needs nothing but the slots, so the scheduler and the
      exporter read the entry itself. `project::VERSION` is 3 with the pattern
      inline per slot; a version 2 document resolves its names against its own
      `rhythms` and hands each slot its own copy, so opening an old file fixes
      the sharing retroactively.
- [x] **`[Copy Sinko]` / `[Paste Sinko]`** (`Shift+Q` / `Shift+J`): copy the
      selected chord's rhythm (grid, hits, hold, muted tail — not its offset,
      which is placement) and paste it onto another chord as an independent copy,
      one undoable edit each. The paste row names what is waiting. Shift selects
      a second action on one physical position, exactly as redo rides on undo, so
      `q`/`j` still move whole entries; the rhythm pair works in the Progression
      panel as well as Sinko, because that list is where you replicate between
      chords, and the Progression header shows the result.
- [x] **Recording implies a rhythm.** A take tapped on a chord with no rhythm
      gives it one built from the take, rather than recording into nothing.
- [ ] **Session persistence.** A chord's rhythm is now part of the session
      document, and the session is only persisted by exporting it. Worth an
      autosaved session file (or a `progressions/` restore-on-launch), because
      "I set up sinko on eight chords" currently dies with the process.

- [x] **Sinko panel**: tap a one-bar rhythm on `$` (the top-left key, a
      `KeyPosition::TopLeft` that never enters the held set), stack takes, and
      assign the result to the chord the Progression panel has selected. The bar
      grid is drawn at the chosen resolution (4/8/16/32/64 steps per bar).
- [x] **Takes stack and decay**: each committed take is a layer, the newest at
      full level and older ones one `TAKE_DECAY` step down, capped at the four
      stab groups the synth provides. `smooth N` (`←`/`→`, defaults to 1 take per
      layer) averages the last N takes into the top layer instead, for converging
      on one clean figure.
- [x] **Pattern library**: `rhythms.toml`, mirroring `patches.toml`, with five
      built-ins; `[Save Pattern As...]` writes it and assigns it in one step.
- [x] **Chord offsets**: `←`/`→` in the Progression panel — and the Sinko
      `offset` row — moves the selected chord along a fixed note ladder (1/32 ·
      1/16 · 1/8 · 3/16 · 1/4 · 3/8 · 1/2 · 3/4 · whole), clamped to a whole note
      either way, and deliberately independent of the pattern's grid. The row
      shows the note value and the ticks: `-1/8  (480 ticks)`.
- [x] **One arrangement seam**: `arrangement.rs` is the only place that decides
      when a note starts, so the scheduler and the MIDI exporter cannot drift.
      Holds that cross a bar line, and chords that spill past the loop, wrap in
      both.
- [x] **One Synth panel**: the four synth subtabs collapsed into a single table
      of all 39 settings, three channel columns wide. `←/→` moves the column,
      `Shift+←/→` nudges the value, `Enter` edits and `Esc` reverts.
- [x] **The session document is version 2** (patterns and offsets, with the
      referenced patterns embedded). Version 1 documents still import.
- [x] **`Quarters` was wrong**: `x---` on a 4-cell grid is one chord per bar, not
      four quarter notes. It is `xxxx` now.
- [x] **Held notes**: a hit can ring from a 32nd up to the whole bar. `Held
      Half` and `Held 3/4` ship, and the Sinko `hold` row walks the note ladder.
      The hold is stored in **ticks**, not as a fraction of a grid cell, so a
      `quant` change re-quantizes the hits without moving the note length.
- [x] **Muted bar tails**: `mute_ticks` (0..=a quarter note) silences the end of
      the bar — nothing starts inside it and anything ringing into it is cut.
      Applied in `arrangement`, so playback and export agree.
- [x] **`Esc` stops, twice quits**: the first press stops the transport and
      silences it at once (a red `STOPPED` banner across the title), the second
      within half a second exits. New `Transport::stop_now` abandons the bar
      instead of letting it finish, so a chord cannot ring on for a bar.
- [x] **Hit editing** (`hits` row): `←`/`→` walks a cell cursor over the grid
      (inverted in place, so the bar still reads as hits and rests) and `Enter`
      toggles a hit — off clears the cell in every take, on puts it in the newest
      one — through to the library, so it is heard on the next bar. Also added the
      2-cell (half note) grid, which `VALID_STEPS` was missing.
- [x] **Reverted: the figure cycle.** An earlier `cells` row walked every way of
      filling the grid, densest first. It was the wrong operation: at 32 and 64
      steps the list runs to billions, and what a take you almost like needs is a
      hit taken *out*, not a different figure swapped in. `hits` replaced it, and
      `MAX_FIGURE_CELLS` / `cell_figures` / `figure_order` are gone.
- [x] **Durations are ticks on a note ladder**: `hold` is ticks, not a fraction
      of a grid cell, so it survives a `quant` change; `offset`, `hold` and
      `mute` all step along 1/32 · 1/16 · 1/8 · 3/16 · 1/4 · 3/8 · 1/2 · 3/4 ·
      whole, and a value off the ladder snaps to the nearest rung.
- [x] **Found and fixed: panel edits were inaudible.** The Sinko rows edited a
      draft that nothing played — the scheduler reads the library — so a hold or
      a mute changed the numbers on screen and nothing else until
      `[Save Pattern As...]`. Edits now write through to the library and the file
      immediately, `[New Pattern]` creates a named assigned pattern, and the
      shape rows refuse when nothing is assigned instead of silently doing
      nothing audible.
- [x] **Replace a slot's chord from the registers** (`b`, Progression panel):
      swaps the degree, transformation and register snapshot in place and keeps
      the entry's rhythm pattern and offset, which is what delete-and-insert
      would lose. One undoable edit; a rest becomes a chord.
- [x] **Found and fixed: `Esc` never reached an open editor.** The quit check ran
      *before* the edit dispatch, so `Esc` left the program instead of reverting a
      BPM, track-key or Synth value, and the handlers' own `Esc` arms were
      unreachable from the event loop — only their direct-call tests covered them.
      Prompts and edits now take `Esc` first.
- [x] **Metronome**: `&` (next to `$`) toggles a click from any panel, plus a
      `metronome` row on the Transport panel. It runs with the transport stopped
      too, and an armed take forces it on without forgetting your own switch.
- [x] **Space is the both-hands chord latch** (`Registers::lock_both`), and the
      transport moved to `{`: once plays/pauses, twice restarts from bar 1,
      three times seeks to the middle. `Enter`/`←`/`→` on the `playing` row do
      the same. The lock hint line now prints the keycap *and* the character the
      layout types, which retires the "lists the registers backwards" bug.

## 0. Done this session

- [x] Fixed the two compile errors that made `cargo build` fail:
  - `src/synth.rs:796` — `capture_patch` omitted the `note_length` field added
    to `MixerPatch`. It now takes `note_length` as a parameter, and the caller
    in `tui.rs` passes `transport.note_length()` directly instead of patching
    the struct afterwards.
  - `src/transport.rs:197` — `live` was partially moved into `BarAction` and
    then borrowed again when advancing the bar counter. Hoisted a
    `live_present: bool` before the match.
- [x] Removed two unused imports in `progression.rs`
  (`std::collections::BTreeSet`, `KeyPosition`).
- [x] Verified the suite: **92 tests pass**.
- [x] Fixed `Enter` not committing the current chord. `Enter` was panel-scoped:
      it only added a chord when the Progression panel had focus, and the app
      starts focused on Transport, where `Enter` opens the BPM editor instead.
      It now commits the resolved chord (latched registers + live keys, live
      winning per side) from any panel, and falls through to the panel action
      only when nothing resolves. Also fixed the `Chord:` readout, which read
      `state.held` directly and so ignored latched registers, and added `Enter`
      to the on-screen help line, which never mentioned it.
      Covered by 13 new tests in `tui.rs` (**105 total**).
- [x] Built the below-home-row hotkey row (it was documented in `keyboard.rs` but
      only the two register locks were ever implemented; the other eight keys
      were wrongly inserted into the held set). Added `Hotkey` +
      `KeyPosition::hotkey()`, folding in the old `LockTarget`, and routed the
      row to a dispatcher that never touches the held set. Ctrl/Alt are now
      ignored by the chord grammar so they stay free for bindings.
- [x] Progression **delete / copy / paste / undo / redo** on `k` / `q` / `j` /
      `x` / `Shift+x`, scoped to the Progression panel, with a bounded
      (128-deep) undo/redo stack in `Progression` that snapshots every mutation.
- [x] Made `ProgressionEntry.registers` meaningful: it now captures the
      *resolved* gesture (latches plus live keys) instead of only the latch
      state — which is why it was previously unreadable — and drives
      `g`-to-recall.
- [x] Recall is explicit rather than automatic on selection, so scrolling the
      progression can no longer clobber a latched register.
      Suite is now **140 tests**.

## 1. Progression editing — remaining work

Done: the below-home-row hotkey row is live, and delete / copy / paste / undo /
redo all work and are undoable. See the README for the key table.

`RightInnerBelow` (`b`) became **replace the selected slot with the registers**,
keeping that entry's pattern and offset. Still open, with three below-home-row
slots reserved (`m`, `,`, `.` on the keycaps — you type `m`, `w`, `v`):

- [ ] Bind `move_up` / `move_down`. Both are implemented, undoable and tested,
      but nothing reaches them. Natural home: `RightIndexBelow` (`m`) /
      `RightMiddleBelow` (`w`).
- [ ] Wire clear-all. `delete_all` is implemented, undoable and tested, and the
      two-stage `ConfirmDeleteAllStage1`/`Stage2` modals still exist for it, but
      `Stage1` is never constructed. Clear-all is destructive enough to keep a
      confirm even with undo available — reach it from `RightRingBelow` (`v`).
- [ ] Decide whether the clipboard should survive a `delete_all`, and whether it
      should be visible in the panel.
- [ ] Multi-select / block operations were deliberately left out of the first
      pass; revisit only if editing one row at a time proves painful.
- [ ] Remove the now-reserved-but-unused `ConfirmDeleteAllStage1` warning by
      wiring it, or delete the modal pair and drop `delete_all`.

## 2. Recall is bound to `g`

Resolved. Selecting a row with `↑`/`↓` no longer touches the registers, so
scrolling can't clobber a latched register mid-performance. Recall is explicit:
`g` (physical `g`, you type `i`) loads the selected entry's register snapshot
and voices it, scoped to the Progression panel.

- [x] Moved the register restore off selection and onto an explicit hotkey.
- [x] `LeftInner` is no longer a chord key. It was previously worse than inert:
      the grammar matches exact shapes, so holding `g` with `f` produced
      `[LeftIndex, LeftInner]`, matched no arm, and silently killed the chord.
      Regression test in `grammar.rs`.
- [ ] Decide whether recall should stay latching. Today it replaces both
      registers and persists, like the lock keys. A momentary
      "hold `g` to preview, release to restore" variant is possible if
      latching over your performance state turns out to be annoying.

## 3. Repository hygiene

- [ ] `git rm --cached debug.log`. It is already in `.gitignore` but was
      committed before that, so it is still tracked and gets truncated and
      rewritten on every launch (~113k lines / 2.7 MB per session).
- [ ] Confirm `patches.toml` should stay tracked. It is user state that the app
      rewrites on "Save As", so expect noisy diffs; consider shipping the
      built-ins in code and keeping the file local.
- [ ] Add a `LICENSE` — the repo currently has none.
- [ ] Clean up `bugs.txt`: three of the four lines are empty `bug0 ::::` stubs.

## 4. Terminal and rendering

- [x] The Synth panel no longer has four subtabs listing 15 parameters at once:
      it is one table, all 39 settings visible, 16 lines. That took the tallest
      view from a focus-dependent 46 down to a constant 48 (still over the old 45
      budget — see the README), and it clears + reprints the whole screen every
      frame.
- [ ] Diffed rendering, so a frame does not clear and reprint everything.
      Options: redraw only when state changes, or adopt `ratatui`.
- [x] **Registers under their hands, chord above the chord list.** `L` and `R`
      start in the columns of the keys they describe, so the row reads as two
      labels under the keyboard; the block's width is a constant
      (`HEADER_BLOCK_WIDTH`) so a long chord label cannot shuffle the keyboard
      sideways. What the registers add up to moved out of that block to the left
      margin, directly above the Progression panel that plays it. **Found on the
      way:** `KEY_COLUMN_GAP.len()` counted *bytes*, and the bar between the hands
      is three of them — the register row was two columns right of its hands.
      `KEY_COLUMN_GAP_WIDTH` is written down and `the_keyboard_layout_adds_up`
      checks it against what `draw_key` draws.
- [x] **The header block is centred too.** Title, keyboard and registers all sit
      on the window's axis: the keyboard is a fixed shape so its centring is exact,
      and the register block is centred as one unit with the chord readout on a
      fixed-width field, so the block cannot change width and slide sideways as
      chords change. `the_centred_header_does_not_move_when_the_chord_changes`
      pins that.
- [x] **The pair spans the window, headings centred, minimum 80.** The chord list
      sits at the left margin and the transport against the right edge, so a wide
      window is used rather than left half-empty; the title is centred on the
      window and each panel's rule is centred over its own body (filled with `─`,
      `━` when focused). The minimum dropped from 100 to 80 columns — the width at
      which the widest fixed row still fits (the grid at its smallest budget, the
      synth table, a pair of columns with the transport at its floor).
- [x] **The layout reads the window.** `Screen::read()` gives the renderer the
      real size each frame. **100 columns is the design width, not a floor to
      reflow into**: below it the frame is replaced by a line naming the window
      and what it needs (`MIN_SCREEN_WIDTH`), because the panels are fixed grids
      and reflowing them would mean cutting data rather than arranging it. At and
      above 100 columns `WidthLimited` cuts every outgoing row to the window — so
      content that overflows, like a long rhythm name or an export path, gets an
      ellipsis instead of a wrap — a pair that cannot fit stacks, and the spare
      columns go to the bar grid (up to 120).
      `no_line_exceeds_the_window_at_any_width` checks the whole range.
- [x] Every Transport row now responds: bpm, loop, metronome, playing, track
      key and mute all react to `←`/`→`, and `Enter` reaches the three buttons
      plus the play row. The old note that rows 0 and 1 ignored the adjustment
      is stale.
- [ ] The Synth table's rows are all covered by tests; the Transport's rows are
      covered by name but not row-by-row — worth a loop over `0..TRANSPORT_ROWS`
      asserting each one changes something.

## 5. Small correctness / tidiness

- [ ] Below-home-row keys other than `z` and `/` are inserted into the held
      `PositionSet` at `tui.rs:810`, contradicting the module doc in
      `keyboard.rs` ("Below-home-row positions are never inserted"). Either
      filter them at the call site or correct the comment.
- [ ] `KeyPosition::LeftInner` is never referenced by `grammar.rs`, so the `g`
      key does nothing. Either give it a grammar meaning or say so in the README.
- [x] **The chime feature is gone.** `chime.rs`, the 4-bar idle cadence, the
      `last chime` Transport row, `chime_index` / `chime_bar_count` /
      `chime_suppressed` and the `PlayChime` / `RecordChime` events are all
      removed, along with the synth's `play_chime` / `stop_chime` /
      `duck_progression` / `duck`. The metronome click kept its own voice — it
      used to borrow one of the chime's three — so the pool is now a single
      `click` voice on the mid channel, summed outside the progression's mute and
      gain. Three of the standing warnings went with it.
- [ ] `Scheduler.transport` is a public field that is never read. Remove or use
      it.
- [ ] `Registers::is_empty` and `PatchStore::names` are unused public API.

## 6. Next features (once the above is clear)

- [ ] Save and load progressions, not just patches — nothing persists the
      progression across restarts (`rhythms.toml` and `patches.toml` do survive).
- [x] Export a progression as MIDI. Done: `[Export MIDI]` on the Transport
      panel writes a timestamped format 0 file, one track on channel 1, with
      tempo / 4/4 / key signature and the progression's chord tones. See
      `MIDI_EXPORT_PLAN.md`.
- [x] Import an exported MIDI back into the session. Done: `[Import MIDI]`
      opens a prompt pre-filled with the newest export; the key, BPM and note
      length are restored along with the progression, and the whole thing is
      one undoable edit. The session is embedded at export time as a versioned
      TOML document in a sequencer-specific meta event (`project.rs`), so
      degrees, transformations and register gestures survive. A file without
      that payload is refused rather than guessed at — which means files
      exported before the payload existed cannot be imported.
- [ ] MIDI export follow-ups, in the order they are worth doing:
  - [ ] Route low / mid / high to separate MIDI channels. The score already
        tags every note with a layer and `smf::TrackLayout::PerLayer` already
        writes the format 1 file; what is missing is a `ChannelMap` on
        `MixerPatch` and a UI to edit it.
  - [ ] Live MIDI output. Add a `MidiSink` trait and a `midir`-backed sink on
        the scheduler events at `tui.rs` (`SchedulerEvent::PlayChord` /
        `StopChord`). The `midi::Score` model is the shared seam; no timing
        engine is needed.
  - [ ] Refactor `synth::allocate` onto `midi::split_layers` so the audio and
        MIDI definitions of low / mid / high cannot drift.
  - [ ] Optional "legato" export that holds each chord to the bar line, and
        optional inclusion of the live bar or a count-in.
  - [ ] MIDI clock / transport sync and CC automation from the mixer.
- [ ] Undo/redo for progression edits.
- [x] Swing / subdivision, and more than one bar per chord. Done as rhythm
      patterns: `arrangement.rs` is the timing seam, so **swing** and
      **multi-bar patterns** are now fields on `RhythmPattern` rather than new
      machinery. Still open.
- [ ] Rhythm follow-ups, in the order they are worth doing:
  - [ ] Swing/shuffle on a pattern (delay every second grid cell).
  - [ ] Patterns longer than one bar.
  - [ ] A per-assignment gain, so one pattern can sit quieter on one chord.
  - [ ] Click a step on and off in the Sinko grid, for fixing a mis-tap.
  - [ ] Audition a pattern from the Progression panel without assigning it.
  - [ ] Route rhythm hits to live MIDI output. `arrangement.rs` already emits
        the note boundaries a sink would consume.
- [ ] Extend the grammar to seventh-scale-degrees so minor-key diatonic
      functions beyond natural minor are reachable.
