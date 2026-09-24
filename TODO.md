# TODO

Short-term next steps for `chord-tool`, in priority order.

**Keyboard layout is fixed to Programmer Dvorak for this version.** It is not
configurable and that is deliberate — no runtime layout switching. (The
on-screen labels showing QWERTY names is a known cosmetic rough edge, not a
scheduling item.)

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

Still open, with four below-home-row slots reserved (`b`, `m`, `w`, `v` on the
keycaps — you type `b`, `m`, `w`, `v`):

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

- [ ] The UI requires ~40 rows because the Synth Mixer panel lists 15 parameters
      at once, and it clears + reprints the whole screen every frame. Options:
      paginate or two-column the mixer, redraw only when state changes, or adopt
      `ratatui` for diffed rendering.
- [ ] Handle a too-small terminal explicitly (clear message instead of a
      scrolled, wrapped mess).
- [ ] `←`/`→` on the Transport rows 0 and 1 ignore the row when adjusting at
      `tui.rs:1003` — verify every focus/row pair actually responds, since
      `adjust_current` silently does nothing on several rows.

## 5. Small correctness / tidiness

- [ ] Below-home-row keys other than `z` and `/` are inserted into the held
      `PositionSet` at `tui.rs:810`, contradicting the module doc in
      `keyboard.rs` ("Below-home-row positions are never inserted"). Either
      filter them at the call site or correct the comment.
- [ ] `KeyPosition::LeftInner` is never referenced by `grammar.rs`, so the `g`
      key does nothing. Either give it a grammar meaning or say so in the README.
- [ ] `Scheduler.transport` is a public field that is never read; the
      `duck_progression` / `stop_chime` / `duck` methods are also unused. Remove
      or use them — `duck` looks intended for a smooth live-chord-to-progression
      handoff.
- [ ] `Registers::is_empty` and `PatchStore::names` are unused public API.

## 6. Next features (once the above is clear)

- [ ] Save and load progressions, not just patches — nothing persists the
      progression across restarts.
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
- [ ] Swing / subdivision, and more than one bar per chord.
- [ ] Extend the grammar to seventh-scale-degrees so minor-key diatonic
      functions beyond natural minor are reachable.
