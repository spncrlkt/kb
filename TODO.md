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

## 1. Wire up the progression editing that already exists

`progression.rs` implements and tests `copy`, `paste_after`, `move_up`,
`move_down`, `delete`, and `delete_all`, but the compiler reports all of them as
never used. The modals `ConfirmDelete` and `ConfirmDeleteAllStage1` are never
constructed, so **there is currently no way to delete a chord from the UI**, and
no way to reorder or duplicate one.

- [ ] Bind copy / paste / move-up / move-down to keys in the Progression panel
      and document them in the on-screen help line.
- [ ] Reach `ConfirmDelete` (e.g. `Delete` on a selected row) and
      `ConfirmDeleteAllStage1` (e.g. `Ctrl+Delete`) so both exist in the state
      machine.
- [ ] Decide whether `Clipboard` should survive a `delete_all`.

## 2. Fix the register snapshot

`ProgressionEntry.registers` is populated at `tui.rs:1120` and never read. Either:

- [ ] Use it — store how a chord was voiced so re-voicing on key change can
      restore the intended register, or show it in the progression row; or
- [ ] Drop the field and simplify `ProgressionEntry`.

Leaving it in place is the worst option: it looks load-bearing but isn't.

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
- [ ] Export a progression as MIDI.
- [ ] Undo/redo for progression edits.
- [ ] Swing / subdivision, and more than one bar per chord.
- [ ] Extend the grammar to seventh-scale-degrees so minor-key diatonic
      functions beyond natural minor are reachable.
