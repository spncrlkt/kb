# MIDI Export — Plan

Export a progression as a Standard MIDI File (`.mid`) that Ableton can import,
structured so that (a) live MIDI output and (b) per-layer MIDI channels can be
added later without reshaping the pipeline.

## Decisions locked

| Question | Choice |
| --- | --- |
| Export layout | **One track, channel 1** for v1. Layered data is still kept in the model; the writer decides layout, so 3-track / per-channel output is a later option. |
| Voicing fidelity | **Pure chord tones.** Do not export the synth's invented octave doublings. |
| SMF writer | **Hand-rolled, zero new dependencies.** |
| Trigger | **Transport panel button row**, `[Export MIDI]`, activated with Enter. **No hotkey.** |
| Filename | `progression-YYYY-MM-DD_HH-MM-SS.mid`, local time, in the working directory. |
| Local time source | **`chrono`** — the only new dependency. |
| Note gaps | Mirror playback: note-off at `note_length × bar`, leaving the rest silent. |

## Why this is cheap

The pieces already exist; the exporter is mostly a pure function over them.

| Need | Already in the code |
| --- | --- |
| Absolute pitches | `ChordSpec::voice(key)` / `diatonic_triad` in `src/music.rs` |
| The loop as a list of bars | `Progression::slots` in `src/progression.rs` |
| Notes per slot | `Slot::notes(&key)` at `src/progression.rs:101` |
| Tempo / key / note length | `Transport::{bpm, key, note_length}` in `src/transport.rs` |
| The low/mid/high concept | `allocate()` at `src/synth.rs:486` |
| A button-row precedent | `[Save As...]` at `src/tui.rs:1460–1466` |

The only genuinely new material is a neutral event model, an SMF byte writer, and
one button row.

## Architecture

The change inserts one pure layer between the progression and any output:

```
Progression + Key + bpm + note_length
        │
        ▼
   midi::Score            ← pure, no I/O, no audio, no terminal
        │
        ├── smf::write()  → progression-<timestamp>.mid   (this work)
        └── live::send()  → MIDI port                     (later, same events)
```

Non-goals for v1: live output, MIDI clock/transport sync, CC automation,
velocity editing, multi-bar chords, non-4/4 meters, configurable export path.

## New module: `src/midi.rs`

The neutral model. No `cpal`, no `crossterm`, no filesystem.

```rust
pub const PPQ: u16 = 960;          // divides cleanly by 2, 3, 4, 5
pub const BEATS_PER_BAR: u64 = 4;  // matches Transport's 4/4 assumption
pub const BAR_TICKS: u64 = PPQ as u64 * BEATS_PER_BAR;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layer { Low, Mid, High }

/// The seam for "low/mid/high on different MIDI channels".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChannelMap { pub low: u8, pub mid: u8, pub high: u8 }

/// Pure chord-tone split — the layer *concept*, with no octave invention.
/// Mirrors `synth::allocate` for 3+ note chords and stays literal for fewer.
pub fn split_layers(notes: &[u8]) -> [Vec<u8>; 3];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub start: u64,     // ticks from the start of the loop
    pub duration: u64,  // ticks; note_length × BAR_TICKS
    pub note: u8,       // 0..=127
    pub velocity: u8,   // constant default for v1
    pub layer: Layer,
}

pub struct Score {
    pub ppq: u16,
    pub bpm: u16,
    pub beats_per_bar: u8,
    pub key: Key,
    pub notes: Vec<Note>,
}

/// The whole loop as timed notes. Excludes the scheduler's live "+1 bar".
pub fn render_progression(
    prog: &Progression,
    key: &Key,
    bpm: u16,
    note_length: f32,
) -> Score;
```

Two deliberate details:

- **Ticks come from BPM directly.** Do *not* derive them from
  `Transport::bar_duration()` — that method does integer microsecond
  arithmetic (`60_000_000 / bpm * 4`) and would accumulate drift across a file.
- **The live bar is excluded.** While playing, the scheduler appends the held
  chord as one extra bar (`src/transport.rs:208`). The export is a
  deterministic function of the progression only.

## The layer → channel seam

`split_layers` is the single source of truth for the layer concept. The
important subtlety: `synth::allocate` **invents octave doublings** for 1- and
2-note chords (`n ± 12`, `src/synth.rs:494–507`). That is sound design, not
musical content, so the MIDI split uses pure chord tones.

For v1 every `Note` still carries its `Layer`, and `ChannelMap` still exists —
they are simply projected to "one track, channel 1" by the writer. Switching to
per-layer channels later is a writer change plus surfacing `ChannelMap` as a
`MixerPatch` field; no data is lost in the meantime.

Recommended follow-up (not v1): refactor `synth::allocate` to call
`split_layers` and add the audio-only doubling on top, so audio and MIDI can
never drift conceptually.

## New module: `src/smf.rs`

Hand-rolled Standard MIDI File writer: a `Score` in, file bytes out, no I/O.

```rust
pub enum TrackLayout {
    /// v1: every layer merged into one track on one channel.
    Single { channel: u8 },
    /// Later: one track per layer, on per-layer channels.
    PerLayer { channels: ChannelMap },
}

pub struct SmfOptions {
    pub layout: TrackLayout,
    pub track_name: String,
}

pub fn write(score: &Score, opts: &SmfOptions) -> Vec<u8>;
```

Details:

- `TrackLayout::Single` writes a **format 0** file — one track, so Ableton
  imports exactly one clip. `PerLayer` writes **format 1** (a conductor track
  plus one track per layer), which is the correct shape once channels differ.
- In format 0 the conductor metadata and the notes share the one track; in
  format 1 track 0 carries the metadata alone.
- Header chunk `MThd`, division = `PPQ` (960).
- Conductor metadata: track name, tempo meta (`60_000_000 / bpm` µs per
  quarter, saturated at the 24-bit ceiling), 4/4 time signature, key signature.
- Note-off is an explicit `0x80` event (not note-on velocity 0), which every
  DAW reads unambiguously.
- Variable-length quantity delta times. Every event — meta included — carries
  its own delta; the end-of-track delta is the one that carries any trailing
  rest out to the loop length.
- At the same tick, emit **note-offs before note-ons** so repeated or legato
  notes retrigger correctly.

## TUI integration: the `[Export MIDI]` button

### Placement

The **Transport** panel gains a seventh row, index 6, at the bottom (below the
read-only `last chime` row). Rendered like the Presets panel's `[Save As...]`
row (`src/tui.rs:1460–1466`):

```
── Transport  (tab to switch) ──
  ▸ bpm                120
    loop               on
    playing            ▶ bar 1/4
    track key          C major
    mute progression   off
    last chime         —
  ▸ [Export MIDI]      progression-2026-09-23_18-03-45.mid
```

The current outcome is shown beside the button, green on success and red on
failure, because flashing is invisible on the Transport panel (it only affects
the Progression panel's empty-state text).

### Plumbing changed

| Site | Before | After |
| --- | --- | --- |
| `row_count` | `Focus::Transport => 5` | `=> TRANSPORT_ROWS` (7) |
| `current_row` | `mixer_row.min(4)` | `mixer_row.min(TRANSPORT_ROWS - 1)` |
| `adjust_current` | rows `0,1,3,4` act | row 6 falls through to no-op |
| `primary_action` | rows `0,1,3,4` | row 6 → `export_midi(state, logger)` |
| `render_transport_panel` | drew rows 0–5 | also draws row 6 |

`mixer_row` is **shared** between `Focus::Transport` and `Focus::SynthMixer`.
That already meant the renderer could highlight nothing after the mixer cursor
was left deep, because it read `mixer_row` while navigation clamped through
`current_row`. The renderer now uses `current_row` too, and a test covers it.

### Enter precedence — resolved

`handle_panel_key` checked `enter_commits_chord(state)` *before* falling through
to `primary_action`, so a held chord would have swallowed the button press and
added a chord instead of exporting (the Presets `[Save As...]` row had the same
latent trap).

This is now decided by a pure `enter_intent(state, ctrl)`:

- Ctrl+Enter → always commit.
- Focused row is an action button → `PanelAction` (the button wins, even with
  keys held).
- A chord resolves → commit it (after the selected row in the Progression
  panel, appended elsewhere).
- Otherwise → `PanelAction`.

Because it is a pure function of `AppState`, the rule is unit-tested without
needing a `Synth` or an audio device.

### Export action

```
fn export_midi(state: &mut AppState, logger: &Logger)
```

1. Snapshot the progression, `key`, `bpm` and `note_length` under the lock,
   then release it before any I/O.
2. Empty progression → write nothing; record and log "nothing to export".
3. `midi::render_progression(...)` → `Score`.
4. `export::export(&score, &state.export_dir, Local::now())`, which serialises
   with `TrackLayout::Single { channel: 0 }` and writes the file.
5. Record the file name (or the error) on the state for the panel, flash, and
   log the absolute path.

`export_dir` lives on `AppState` rather than calling `current_dir()` at export
time, so tests write into a temp directory and it can become a setting later.

No file I/O ever happens on the audio callback — this runs on the UI thread on
an explicit keypress.

## Filename

`progression-2026-09-23_18-03-45.mid`, from local wall-clock time:

```rust
pub fn export_filename(now: chrono::DateTime<chrono::Local>) -> String {
    format!("progression-{}.mid", now.format("%Y-%m-%d_%H-%M-%S"))
}
```

Taking `now` as a parameter keeps the formatter a pure, unit-testable function
with no clock dependency in the test. Seconds resolution means two exports
within the same second overwrite; acceptable, and a `-2` suffix is a trivial
later fix if it ever bites.

Dependency: `chrono = "0.4"` with default features. `chrono`'s `Local::now()`
uses `libc::localtime_r` on Unix (thread-safe) and handles DST and the platform
timezone database correctly; hand-rolling this is where bugs live. This is the
project's only new dependency for the whole feature — the SMF writer stays
dependency-free.

## Live-output seam (designed now, built later)

Define the consumer shape now so file and live output are interchangeable:

```rust
pub trait MidiSink {
    /// Compute the layer split and emit note-ons.
    fn chord_on(&mut self, notes: &[u8], velocity: u8);
    /// Release everything this sink is holding.
    fn all_off(&mut self);
}
```

- **v1:** `FileSink` accumulates a `Score`.
- **Later:** `LiveSink` wraps a `midir` output port. The existing scheduler
  drain at `src/tui.rs:657` already turns `SchedulerEvent::PlayChord` /
  `StopChord` into calls; forward those to both `synth` and the sink. The
  scheduler owns timing, so live output needs no new clock.
- Known limitation to accept: the UI polls at 5 ms, so live note timing is
  millisecond-ish, not sample-accurate. Fine for auditioning, not for tight
  sync. A dedicated MIDI-clock thread (24 ppqn + Start/Stop) is the later fix.
- Define the message vocabulary broadly enough to grow into `Clock` / `Start` /
  `Stop` / `CC` later, even though v1 only emits note-on/note-off.

## Phases

| Phase | Deliverable | Status |
| --- | --- | --- |
| 1 | `midi.rs`: model, `split_layers`, `render_progression` | **Done** — pure unit tests for `BAR_TICKS`, durations, splits, rests, layers |
| 2 | `smf.rs`: writer + `TrackLayout::Single` | **Done** — an in-test reader parses the bytes back; VLQ, header, tempo, channel and key-signature tests |
| 3 | `[Export MIDI]` button on Transport, `chrono` filename, buttons-win Enter | **Done** — writes `progression-<timestamp>.mid`; verified against an independent parser |
| 4 | `project.rs` session document + `[Import MIDI]` button | **Done** — embedded as an `FF 7F` event, refused if absent; see below |
| 5 | `MidiSink` trait + `FileSink` | Not started |
| 6 *(later)* | `midir` live output; `ChannelMap` on `MixerPatch` + mixer UI | Not started |
| 7 *(later)* | MIDI clock/transport, CC from the mixer | Not started |

## What shipped

New modules:

| File | Responsibility |
| --- | --- |
| `src/midi.rs` | Pure model: `Score`, `Note`, `Layer`, `ChannelMap`, `split_layers`, `render_progression`. |
| `src/smf.rs` | Pure SMF writer **and reader**: `TrackLayout`, `SmfOptions`, `write`, `read_project`, `SmfError`. |
| `src/project.rs` | The versioned session document: `Project`, `SlotDoc`, `encode`, `decode`, `restore`. |
| `src/export.rs` | Filesystem only: `export_filename`, `write_score`, `export`, `import`, `ImportError`. |

Changed: `src/tui.rs` (Transport button rows, `enter_intent`, `export_midi`,
`import_midi`, `open_import_modal`, `ActionStatus`, `export_dir` on `AppState`),
`src/progression.rs` (`replace`, one undoable edit), `src/music.rs` and
`src/keyboard.rs` (`serde` derives on the enums the document stores),
`src/main.rs` (module declarations), `Cargo.toml` (`chrono`).

Suite: **221 tests**, up from 140. No new compiler warnings.

Two writer bugs were caught by the in-test SMF reader rather than by the DAW:
a missing delta-time byte on every meta event, and a doubled delta before the
end-of-track marker. Both would have produced a file that opened but parsed
wrong. The final artifact was also checked with `file` (reports
`Standard MIDI data (format 0) using 1 track at 1/960`) and an independent
Python parser.

## Importer

### The problem it has to solve

A MIDI file holds absolute notes and nothing else. Degrees, transformations and
register gestures **cannot be recovered** from them: C-E-G is I in C, IV in G
and V in F, and all three export the same three note numbers. Any reconstruction
would silently disagree with what was played, which is worse than refusing.

So the exporter embeds the session, and the importer reads it back. Detection
and fidelity are therefore both decided at *export* time.

### Payload format

A **sequencer-specific meta event** (`FF 7F`) in the conductor track:

```
00 FF 7F <vlq length> 7D "CTP1" <TOML document>
```

- `7D` is the MIDI manufacturer id for non-commercial/educational use.
- `CTP1` is the magic. A reader can therefore *positively identify* our files
  instead of heuristically guessing from track names.
- `FF 7F` rather than a text/lyric meta event on purpose: DAWs render text
  events, so a TOML blob would appear in the UI. Sequencer-specific data is
  ignored by every DAW while remaining trivially detectable by us.

The document is TOML (the crate already depends on `toml`) and versioned:

```toml
version = 1
key_tonic = 60
key_scale = "major"
bpm = 120
note_length = 0.5

[[slots]]
kind = "chord"
degree = "i"
transformation = "diatonic7"
left = ["left_index"]
```

Design points:

- **`None` vs `Some([])` registers are preserved.** "Never set" and "explicitly
  cleared" resolve differently, so flattening them would change `g`-to-recall
  behaviour after a round trip. `skip_serializing_if` drops `None` entirely.
- **Wire names are the `serde` variant names** of `Scale`, `ScaleDegree`,
  `Transformation` and `KeyPosition`, in `snake_case`. These are now part of the
  file format, so renaming a Rust variant is a breaking change that must bump
  `project::VERSION`. A test walks *every* variant of `Transformation` and
  `KeyPosition` to prove the names are unique and round-trip — the manual
  `Transformation::label()` is not usable here, because two variants
  (`Thirteen` and `Diatonic7_13`) both label as `"13"`.
- **Unknown slot kinds, a chord with no degree, and a different version are all
  refused** with specific errors rather than best-effort parsing.

### Import flow

`[Import MIDI]` opens a prompt pre-filled with the newest `progression-*.mid`
(timestamped names sort chronologically). A relative name resolves against the
export directory; an absolute path is honoured as given. The store is:

- read the file,
- extract the `FF 7F` payload (`smf::read_project`),
- decode it (`project::decode`),
- restore domain values (`project::restore`),
- install: `Progression::replace` (one undoable edit), `set_key`, `set_bpm`,
  `set_note_length`.

`ImportError` distinguishes four failures so the message is useful: I/O, not a
MIDI file, a MIDI file that is not ours, and a document we cannot understand.

### Consequence to be honest about

Exports made **before** this feature have no payload and will be refused. That
is the intended trade: a refusal is recoverable, a silently wrong progression is
not.

## Test plan

- **Tick math:** one bar = 3840 ticks; `note_length` of 1/4, 1/2, 3/4, whole →
  960 / 1920 / 2880 / 3840 tick durations.
- **Rendering:** slots laid end to end one bar apart; `Slot::Rest` and an empty
  progression emit nothing; layers are assigned; trailing rests extend the
  length.
- **Split:** a triad gives one note per layer; a 7th puts two in `Mid`; a single
  note lands in `Low` with no invented octaves; sorting and de-duplication.
- **Filename:** `export_filename` with a fixed timestamp yields
  `progression-2026-09-23_18-03-45.mid`; zero-padding is correct.
- **SMF:** VLQ boundary cases (`0x7F`, `0x80`, `0x3FFF`, `0x0FFFFFFF`); `MThd`
  length and division; tempo meta equals `60_000_000 / bpm` and saturates at the
  24-bit ceiling; per-layer channel routing; note-off before note-on at a bar
  line; parse-back equality.
- **Panel:** `row_count(Focus::Transport) == TRANSPORT_ROWS`; a held chord does
  not swallow either button; a value row still commits the chord; Ctrl+Enter
  always commits; the Presets save row behaves the same way; an empty
  progression writes no file; a failed write is reported; the selection clamps
  after the mixer cursor was left deep.
- **Document:** a chord/rest progression round-trips exactly, including key,
  BPM and note length; `None` and `Some([])` registers stay distinct; every
  `Transformation` and `KeyPosition` variant has a unique wire name; an
  unsupported version, malformed TOML, non-UTF-8 bytes, an unknown slot kind
  and a chord with no degree are each refused; an out-of-range tonic is clamped.
- **Import:** an export imports back to the same session; a plain MIDI file with
  no payload is refused as "not ours"; a non-MIDI file is refused as MIDI; a
  missing file is an I/O error; an empty name is refused without touching the
  disk; an absolute path wins over the export directory; the prompt pre-fills
  the newest export; the whole import is one undo step.
- **Independent:** the generated artifact was parsed by a separate Python
  implementation, using the standard library's `tomllib`, to recover the notes
  and the document without sharing any code with the writer.

## Risks and gotchas

- **Files exported before the payload existed cannot be imported.** They are
  refused with a message that says so. A refusal is recoverable; a silently
  wrong progression is not.
- **A DAW that rewrites or strips unknown meta events will destroy the
  payload.** Re-import from the file our tool wrote, not from one Ableton has
  re-saved. This is inherent to embedding session data in a MIDI file; a sidecar
  file would survive but would not travel with the `.mid`.
- **The wire format is versioned but only v1 exists.** `project::VERSION` must
  be bumped when a Rust variant of `Scale`, `ScaleDegree`, `Transformation` or
  `KeyPosition` is renamed, because those names are the format.
- **`note_length < whole` exports gaps.** Intended — it mirrors playback — but
  surprising if you expected a solid piano roll. A "legato" option is a cheap
  later addition.
- **Two exports in the same second overwrite**, since the filename has seconds
  resolution. Accepted for now; a `-2` suffix is a trivial later fix.
- **Ableton may ignore odd channels on import** when a clip is flattened; this
  is exactly why v1 is single-track/channel 1 and the 3-track option stays
  behind `TrackLayout`.
- **Velocity has no audio analogue** (the synth has no velocity), so v1 writes a
  constant. The field exists so it can be driven later without a model change.
- **4/4 is hardcoded** in the transport today; the score carries
  `beats_per_bar` so a meter feature won't require reworking the writer.
- **`mixer_row` is shared** between the Transport and Synth Mixer panels. The
  renderer now goes through `current_row`, which removes the visible symptom,
  but the shared cursor remains a wrinkle.
- **No new dependency for the SMF writer**, but `chrono` is added for local
  time — a deliberate exception, since timezone handling is not worth
  hand-rolling.

## Open follow-ups

- Support a sidecar document next to the `.mid` as a fallback, so a session
  survives a DAW re-save that strips unknown meta events.
- Offer "import the notes only" as an explicit, clearly-labelled fallback for
  files that are not ours, so foreign MIDI can be used without pretending the
  degrees were recovered.
- Persist the session document independently of audio (also unblocks
  "save/load progressions" in `TODO.md` §6).
- Decide whether `ChannelMap` lives in `MixerPatch` (per-preset routing) or in
  `Transport` (global routing).
- Decide whether export should optionally include a count-in or the live bar.
- Consider a configurable export directory (next to `patches.toml`, say) if the
  working directory proves awkward.
- Refactor `synth::allocate` onto `midi::split_layers` so the audio and MIDI
  notions of "low / mid / high" share one definition.
