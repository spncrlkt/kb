//! Standard MIDI File (`.mid`) writer.
//!
//! Pure byte assembly: a [`Score`] goes in, the bytes of a file come out. No
//! I/O happens here, which keeps the format logic directly testable and keeps
//! filesystem concerns in one place (`crate::export`).
//!
//! Two layouts:
//!
//! - [`TrackLayout::Single`] writes a **format 0** file — one track, every note
//!   on one channel. That is what Ableton turns into a single clip, which is
//!   the current goal.
//! - [`TrackLayout::PerLayer`] writes a **format 1** file with a conductor
//!   track plus one track per layer on its own channel. That is the future
//!   "low / mid / high on separate MIDI channels" mode; the score already
//!   carries the layers, so it needs no model change.

use crate::midi::{ChannelMap, Layer, Score};
use crate::music::Scale;

/// How the score is spread across tracks and channels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrackLayout {
    /// One track, every layer collapsed onto `channel`.
    Single { channel: u8 },
    /// A conductor track plus one track per layer, each on its own channel.
    ///
    /// Not reachable from the UI yet — it is the deliberate seam for routing
    /// low/mid/high to separate MIDI channels, and is exercised by tests.
    #[allow(dead_code)]
    PerLayer { channels: ChannelMap },
}

#[derive(Clone, Debug)]
pub struct SmfOptions {
    pub layout: TrackLayout,
    pub track_name: String,
    /// An opaque project document to embed in the conductor track.
    ///
    /// `smf` only frames it (manufacturer id + magic); what the bytes mean is
    /// `crate::project`'s business.
    pub project: Option<Vec<u8>>,
}

impl SmfOptions {
    /// The current default: a single track on channel 1 (zero-indexed 0).
    pub fn single(track_name: &str) -> Self {
        SmfOptions {
            layout: TrackLayout::Single { channel: 0 },
            track_name: track_name.to_string(),
            project: None,
        }
    }

    /// Embed a project document alongside the notes.
    pub fn with_project(mut self, project: Vec<u8>) -> Self {
        self.project = Some(project);
        self
    }
}

/// Serialise a score to the bytes of a `.mid` file.
pub fn write(score: &Score, opts: &SmfOptions) -> Vec<u8> {
    match opts.layout {
        TrackLayout::Single { channel } => format0(score, opts, channel),
        TrackLayout::PerLayer { channels } => format1(score, opts, channels),
    }
}

// -----------------------------------------------------------------------------
// Layouts
// -----------------------------------------------------------------------------

fn format0(score: &Score, opts: &SmfOptions, channel: u8) -> Vec<u8> {
    // Every layer onto the one channel.
    let channels = ChannelMap {
        low: channel,
        mid: channel,
        high: channel,
    };
    let events = collect(score, &[Layer::Low, Layer::Mid, Layer::High], &channels);

    let mut body = Vec::new();
    conductor_meta(score, opts, &mut body);
    write_events(&events, score.length_ticks, &mut body);

    let mut out = Vec::new();
    push_header(&mut out, 0, 1, score.ppq);
    push_track(&mut out, &body);
    out
}

fn format1(score: &Score, opts: &SmfOptions, channels: ChannelMap) -> Vec<u8> {
    let mut out = Vec::new();
    push_header(&mut out, 1, 4, score.ppq);

    // Track 0: conductor (tempo, meter, key). Ableton shows it as an empty
    // track, which is the standard shape for a format 1 file.
    let mut conductor = Vec::new();
    conductor_meta(score, opts, &mut conductor);
    end_of_track(&mut conductor, 0);
    push_track(&mut out, &conductor);

    for (layer, name) in [
        (Layer::Low, "Low"),
        (Layer::Mid, "Mid"),
        (Layer::High, "High"),
    ] {
        let events = collect(score, &[layer], &channels);
        let mut body = Vec::new();
        meta(
            &mut body,
            0x03,
            format!("{} {}", opts.track_name, name).as_bytes(),
        );
        write_events(&events, score.length_ticks, &mut body);
        push_track(&mut out, &body);
    }

    out
}

// -----------------------------------------------------------------------------
// Events
// -----------------------------------------------------------------------------

/// A note boundary, before delta-times are computed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct RawEvent {
    tick: u64,
    on: bool,
    note: u8,
    velocity: u8,
    channel: u8,
}

/// Flatten the score's notes into note-on / note-off events for the requested
/// layers, in the order they must be written.
fn collect(score: &Score, layers: &[Layer], channels: &ChannelMap) -> Vec<RawEvent> {
    let mut events = Vec::with_capacity(score.notes.len() * 2);
    for note in &score.notes {
        if !layers.contains(&note.layer) {
            continue;
        }
        let channel = channels.channel(note.layer);
        events.push(RawEvent {
            tick: note.start,
            on: true,
            note: note.note,
            // A velocity of 0 would be read as a note-off, so floor it.
            velocity: note.velocity.max(1),
            channel,
        });
        events.push(RawEvent {
            tick: note.end(),
            on: false,
            note: note.note,
            velocity: 0x40,
            channel,
        });
    }

    // At the same tick, note-offs must sort before note-ons so that a repeated
    // note retriggers instead of being cancelled by its own release. `on` is a
    // bool, so `false` (off) orders first.
    events.sort_by_key(|e| (e.tick, e.on, e.note, e.channel));
    events
}

/// Write the event stream as delta-timed events, then end the track.
fn write_events(events: &[RawEvent], end_tick: u64, out: &mut Vec<u8>) {
    let mut previous = 0u64;
    for event in events {
        push_vlq(out, (event.tick - previous) as u32);
        previous = event.tick;

        if event.on {
            out.push(0x90 | (event.channel & 0x0F));
            out.push(event.note & 0x7F);
            out.push(event.velocity & 0x7F);
        } else {
            // An explicit 0x80 note-off, rather than note-on with velocity 0,
            // which every sequencer reads unambiguously.
            out.push(0x80 | (event.channel & 0x0F));
            out.push(event.note & 0x7F);
            out.push(event.velocity & 0x7F);
        }
    }

    let end = end_tick.max(previous);
    end_of_track(out, (end - previous) as u32);
}

// -----------------------------------------------------------------------------
// Chunks and meta events
// -----------------------------------------------------------------------------

fn push_header(out: &mut Vec<u8>, format: u16, tracks: u16, division: u16) {
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&6u32.to_be_bytes()); // header length is always 6
    out.extend_from_slice(&format.to_be_bytes());
    out.extend_from_slice(&tracks.to_be_bytes());
    out.extend_from_slice(&division.to_be_bytes());
}

fn push_track(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

/// Tempo, meter, key, and the embedded project document.
fn conductor_meta(score: &Score, opts: &SmfOptions, out: &mut Vec<u8>) {
    meta(out, 0x03, opts.track_name.as_bytes());

    if let Some(project) = &opts.project {
        project_meta(out, project);
    }

    // Tempo is 24-bit microseconds per quarter note, so it saturates at
    // 0xFFFFFF (~3.6 BPM). Without the clamp a very low BPM would truncate.
    let micros = (60_000_000u32 / score.bpm.max(1) as u32).min(0xFF_FFFF);
    meta(out, 0x51, &micros.to_be_bytes()[1..]);

    // 4/4 time: numerator 4, denominator 2^2, 24 MIDI clocks per click,
    // 8 thirty-second notes per quarter.
    meta(out, 0x58, &[4, 2, 24, 8]);

    let (sharps, minor) = key_signature(score);
    meta(out, 0x59, &[sharps as u8, minor]);
}

/// The manufacturer id for "non-commercial / educational use", which is what
/// third-party sequencer-specific data is supposed to use.
const MANUFACTURER_NON_COMMERCIAL: u8 = 0x7D;

/// Magic marking a payload as a chord-tool project document, so a file can be
/// recognised as ours without attempting a parse.
pub const PROJECT_MAGIC: [u8; 4] = *b"CTP1";

/// Embed the project document as a sequencer-specific meta event.
///
/// Deliberately *not* a text (lyric) meta event: DAWs render those, and a TOML
/// blob would show up in the UI. Sequencer-specific data is ignored by every
/// DAW, which is exactly what this wants, while staying trivially detectable
/// by our own reader.
fn project_meta(out: &mut Vec<u8>, payload: &[u8]) {
    let mut data = Vec::with_capacity(1 + PROJECT_MAGIC.len() + payload.len());
    data.push(MANUFACTURER_NON_COMMERCIAL);
    data.extend_from_slice(&PROJECT_MAGIC);
    data.extend_from_slice(payload);
    meta(out, 0x7F, &data);
}

fn meta(out: &mut Vec<u8>, kind: u8, data: &[u8]) {
    // Every event, meta included, is preceded by a delta time. All meta events
    // here sit at tick 0.
    out.push(0x00);
    out.push(0xFF);
    out.push(kind);
    push_vlq(out, data.len() as u32);
    out.extend_from_slice(data);
}

/// End the track after `delta` ticks of silence.
///
/// The delta belongs here rather than at the call site: emitting it separately
/// would leave two deltas before the end-of-track meta, which a strict reader
/// interprets as an empty running-status event.
fn end_of_track(out: &mut Vec<u8>, delta: u32) {
    push_vlq(out, delta);
    out.extend_from_slice(&[0xFF, 0x2F, 0x00]);
}

/// Sharps (positive) or flats (negative) for the track key, plus major/minor.
///
/// The transport stores only a pitch class, so enharmonic keys are spelled by
/// common practice (D-flat rather than C-sharp, and so on). The key signature
/// is informational — the notes themselves are absolute — so this never
/// affects what plays.
fn key_signature(score: &Score) -> (i8, u8) {
    const MAJOR: [i8; 12] = [0, -5, 2, -3, 4, -1, 6, 1, -4, 3, -2, 5];
    const MINOR: [i8; 12] = [-3, 4, -1, -6, 1, -4, 3, -2, 5, 0, -5, 2];
    let pc = (score.key.tonic % 12) as usize;
    match score.key.scale {
        Scale::Major => (MAJOR[pc], 0),
        Scale::Minor => (MINOR[pc], 1),
    }
}

/// Variable-length quantity, as MIDI files encode delta times.
fn push_vlq(out: &mut Vec<u8>, value: u32) {
    let mut buffer = [0u8; 5];
    let mut index = buffer.len() - 1;
    buffer[index] = (value & 0x7F) as u8;
    let mut rest = value >> 7;
    while rest > 0 {
        index -= 1;
        buffer[index] = ((rest & 0x7F) as u8) | 0x80;
        rest >>= 7;
    }
    out.extend_from_slice(&buffer[index..]);
}

// -----------------------------------------------------------------------------
// Reading
// -----------------------------------------------------------------------------

#[derive(Debug)]
pub enum SmfError {
    /// The file is too short to contain a header.
    TooShort,
    /// The file does not start with a MIDI header, or a track is misplaced.
    NotMidi,
    /// A length field runs past the end of the data.
    Truncated,
}

impl std::fmt::Display for SmfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmfError::TooShort => write!(f, "file is too short to be a MIDI file"),
            SmfError::NotMidi => write!(f, "file is not a Standard MIDI File"),
            SmfError::Truncated => write!(f, "MIDI file is truncated"),
        }
    }
}

impl std::error::Error for SmfError {}

/// Pull the chord-tool project document out of a MIDI file.
///
/// `Ok(None)` means the file is a well-formed MIDI file that this tool did not
/// write — including exports made before the payload existed. Callers should
/// refuse those rather than guess, because scales and transformations cannot be
/// recovered from the notes.
pub fn read_project(bytes: &[u8]) -> Result<Option<Vec<u8>>, SmfError> {
    if bytes.len() < 14 {
        return Err(SmfError::TooShort);
    }
    if &bytes[0..4] != b"MThd" {
        return Err(SmfError::NotMidi);
    }
    let header_len = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if header_len < 6 || 8 + header_len > bytes.len() {
        return Err(SmfError::Truncated);
    }
    let tracks = u16::from_be_bytes(bytes[10..12].try_into().unwrap()) as usize;

    let mut pos = 8 + header_len;
    for _ in 0..tracks {
        if pos + 8 > bytes.len() {
            return Err(SmfError::Truncated);
        }
        if &bytes[pos..pos + 4] != b"MTrk" {
            return Err(SmfError::NotMidi);
        }
        let len = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= bytes.len())
            .ok_or(SmfError::Truncated)?;
        if let Some(payload) = scan_track(&bytes[start..end])? {
            return Ok(Some(payload));
        }
        pos = end;
    }
    Ok(None)
}

/// Walk one track's events looking for our sequencer-specific payload.
fn scan_track(body: &[u8]) -> Result<Option<Vec<u8>>, SmfError> {
    let mut pos = 0usize;
    let mut running = 0u8;

    while pos < body.len() {
        read_vlq(body, &mut pos)?;
        if pos >= body.len() {
            return Err(SmfError::Truncated);
        }
        let status = body[pos];

        match status {
            0xFF => {
                pos += 1;
                if pos >= body.len() {
                    return Err(SmfError::Truncated);
                }
                let kind = body[pos];
                pos += 1;
                let len = read_vlq(body, &mut pos)? as usize;
                let end = pos
                    .checked_add(len)
                    .filter(|end| *end <= body.len())
                    .ok_or(SmfError::Truncated)?;
                if kind == 0x2F {
                    return Ok(None);
                }
                if kind == 0x7F {
                    if let Some(payload) = strip_project_magic(&body[pos..end]) {
                        return Ok(Some(payload));
                    }
                }
                pos = end;
            }
            0xF0 | 0xF7 => {
                pos += 1;
                let len = read_vlq(body, &mut pos)? as usize;
                pos = pos
                    .checked_add(len)
                    .filter(|end| *end <= body.len())
                    .ok_or(SmfError::Truncated)?;
            }
            _ => {
                if status & 0x80 != 0 {
                    running = status;
                    pos += 1;
                }
                if running == 0 {
                    return Err(SmfError::NotMidi);
                }
                // One data byte for program change / channel pressure, two for
                // every other channel message.
                let data = match running & 0xF0 {
                    0xC0 | 0xD0 => 1,
                    _ => 2,
                };
                pos = pos
                    .checked_add(data)
                    .filter(|end| *end <= body.len())
                    .ok_or(SmfError::Truncated)?;
            }
        }
    }
    Ok(None)
}

/// Check the manufacturer id and magic, returning the document that follows.
fn strip_project_magic(data: &[u8]) -> Option<Vec<u8>> {
    let prefix = 1 + PROJECT_MAGIC.len();
    if data.len() >= prefix
        && data[0] == MANUFACTURER_NON_COMMERCIAL
        && data[1..prefix] == PROJECT_MAGIC[..]
    {
        Some(data[prefix..].to_vec())
    } else {
        None
    }
}

fn read_vlq(data: &[u8], pos: &mut usize) -> Result<u32, SmfError> {
    let mut value = 0u32;
    for _ in 0..4 {
        if *pos >= data.len() {
            return Err(SmfError::Truncated);
        }
        let byte = data[*pos];
        *pos += 1;
        value = (value << 7) | (byte & 0x7F) as u32;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(SmfError::NotMidi)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi;
    use crate::music::{Key, Scale, ScaleDegree, Transformation};
    use crate::progression::{Progression, ProgressionEntry, Registers, Slot};

    // ---- a small SMF reader, so tests assert on meaning, not golden bytes ----

    struct Parsed {
        format: u16,
        division: u16,
        tracks: Vec<Vec<u8>>,
    }

    fn parse(bytes: &[u8]) -> Parsed {
        assert_eq!(&bytes[0..4], b"MThd", "missing MThd");
        assert_eq!(u32::from_be_bytes(bytes[4..8].try_into().unwrap()), 6);
        let format = u16::from_be_bytes(bytes[8..10].try_into().unwrap());
        let ntrks = u16::from_be_bytes(bytes[10..12].try_into().unwrap());
        let division = u16::from_be_bytes(bytes[12..14].try_into().unwrap());

        let mut pos = 14usize;
        let mut tracks = Vec::new();
        for _ in 0..ntrks {
            assert_eq!(&bytes[pos..pos + 4], b"MTrk", "missing MTrk");
            let len = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
            tracks.push(bytes[pos + 8..pos + 8 + len].to_vec());
            pos += 8 + len;
        }
        assert_eq!(pos, bytes.len(), "trailing bytes after the last track");

        Parsed {
            format,
            division,
            tracks,
        }
    }

    /// `(tick, is_on, channel, note, velocity)` for every channel event.
    type Event = (u64, bool, u8, u8, u8);

    fn events(body: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        let mut tick = 0u64;
        let mut running = 0u8;

        while pos < body.len() {
            tick += read_vlq(body, &mut pos) as u64;
            let status = body[pos];

            match status {
                0xFF => {
                    pos += 1;
                    let kind = body[pos];
                    pos += 1;
                    let len = read_vlq(body, &mut pos) as usize;
                    if kind == 0x2F {
                        break; // end of track
                    }
                    pos += len;
                }
                0xF0 | 0xF7 => {
                    pos += 1;
                    let len = read_vlq(body, &mut pos) as usize;
                    pos += len;
                }
                _ => {
                    if status & 0x80 != 0 {
                        running = status;
                        pos += 1;
                    }
                    let kind = running & 0xF0;
                    let channel = running & 0x0F;
                    let note = body[pos];
                    let velocity = body[pos + 1];
                    pos += 2;
                    match kind {
                        0x90 if velocity > 0 => out.push((tick, true, channel, note, velocity)),
                        0x80 | 0x90 => out.push((tick, false, channel, note, velocity)),
                        _ => {}
                    }
                }
            }
        }
        out
    }

    fn read_vlq(data: &[u8], pos: &mut usize) -> u32 {
        let mut value = 0u32;
        loop {
            let byte = data[*pos];
            *pos += 1;
            value = (value << 7) | (byte & 0x7F) as u32;
            if byte & 0x80 == 0 {
                return value;
            }
        }
    }

    fn meta_events(body: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < body.len() {
            let _ = read_vlq(body, &mut pos);
            let status = body[pos];
            if status != 0xFF {
                if status == 0xF0 || status == 0xF7 {
                    pos += 1;
                    let len = read_vlq(body, &mut pos) as usize;
                    pos += len;
                } else {
                    if status & 0x80 != 0 {
                        pos += 1;
                    }
                    pos += 2;
                }
                continue;
            }
            pos += 1;
            let kind = body[pos];
            pos += 1;
            let len = read_vlq(body, &mut pos) as usize;
            let data = body[pos..pos + len].to_vec();
            pos += len;
            if kind == 0x2F {
                break;
            }
            out.push((kind, data));
        }
        out
    }

    // ---- fixtures ----

    fn score_with(slots: Vec<Slot>) -> Score {
        let mut prog = Progression::new();
        prog.slots = slots;
        midi::render_progression(&prog, &Key::new(60, Scale::Major), 120, 1.0)
    }

    fn chord(degree: ScaleDegree, transformation: Option<Transformation>) -> Slot {
        Slot::Chord(ProgressionEntry {
            degree,
            transformation,
            registers: Registers::default(),
        })
    }

    fn single(score: &Score) -> Parsed {
        parse(&write(score, &SmfOptions::single("Progression")))
    }

    // ---- VLQ ----

    fn vlq(value: u32) -> Vec<u8> {
        let mut out = Vec::new();
        push_vlq(&mut out, value);
        out
    }

    #[test]
    fn vlq_encodes_the_boundaries() {
        assert_eq!(vlq(0x00), vec![0x00]);
        assert_eq!(vlq(0x40), vec![0x40]);
        assert_eq!(vlq(0x7F), vec![0x7F]);
        assert_eq!(vlq(0x80), vec![0x81, 0x00]);
        assert_eq!(vlq(0x2000), vec![0xC0, 0x00]);
        assert_eq!(vlq(0x3FFF), vec![0xFF, 0x7F]);
        assert_eq!(vlq(0x4000), vec![0x81, 0x80, 0x00]);
        assert_eq!(vlq(0x0FFFFFFF), vec![0xFF, 0xFF, 0xFF, 0x7F]);
    }

    // ---- header ----

    #[test]
    fn single_layout_is_format_0_with_one_track() {
        let parsed = single(&score_with(vec![chord(ScaleDegree::I, None)]));
        assert_eq!(parsed.format, 0);
        assert_eq!(parsed.tracks.len(), 1);
        assert_eq!(parsed.division, midi::PPQ);
    }

    #[test]
    fn per_layer_layout_is_format_1_with_a_conductor_and_three_tracks() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let parsed = parse(&write(
            &score,
            &SmfOptions {
                layout: TrackLayout::PerLayer {
                    channels: ChannelMap::default(),
                },
                track_name: "Progression".to_string(),
                project: None,
            },
        ));
        assert_eq!(parsed.format, 1);
        assert_eq!(parsed.tracks.len(), 4);
    }

    // ---- notes ----

    #[test]
    fn a_triad_round_trips() {
        let parsed = single(&score_with(vec![chord(ScaleDegree::I, None)]));
        let mut found = events(&parsed.tracks[0]);
        found.sort_by_key(|e| (e.3, e.1));
        found.retain(|e| e.1);

        let mut notes: Vec<u8> = found.iter().map(|e| e.3).collect();
        notes.sort_unstable();
        assert_eq!(notes, vec![60, 64, 67]);
    }

    #[test]
    fn note_offs_land_at_the_rendered_duration() {
        let score = midi::render_progression(
            &{
                let mut p = Progression::new();
                p.slots = vec![chord(ScaleDegree::I, None)];
                p
            },
            &Key::new(60, Scale::Major),
            120,
            0.5, // half a bar
        );
        let parsed = single(&score);
        let found = events(&parsed.tracks[0]);
        assert!(found.iter().any(|e| e.1 && e.0 == 0), "note-ons at tick 0");
        assert!(
            found.iter().any(|e| !e.1 && e.0 == 1920),
            "note-offs at half a bar"
        );
    }

    #[test]
    fn every_event_carries_the_single_channel() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let bytes = write(
            &score,
            &SmfOptions {
                layout: TrackLayout::Single { channel: 5 },
                track_name: "Progression".to_string(),
                project: None,
            },
        );
        let parsed = parse(&bytes);
        assert!(events(&parsed.tracks[0]).iter().all(|e| e.2 == 5));
    }

    #[test]
    fn per_layer_routes_each_layer_to_its_own_channel() {
        let score = score_with(vec![chord(ScaleDegree::I, Some(Transformation::Dom7))]);
        let parsed = parse(&write(
            &score,
            &SmfOptions {
                layout: TrackLayout::PerLayer {
                    channels: ChannelMap::default(),
                },
                track_name: "Progression".to_string(),
                project: None,
            },
        ));

        // Tracks 1/2/3 are Low/Mid/High on channels 0/1/2.
        assert!(events(&parsed.tracks[1]).iter().all(|e| e.2 == 0));
        assert!(events(&parsed.tracks[2]).iter().all(|e| e.2 == 1));
        assert!(events(&parsed.tracks[3]).iter().all(|e| e.2 == 2));

        let notes = |track: usize| -> Vec<u8> {
            let mut v: Vec<u8> = events(&parsed.tracks[track])
                .iter()
                .filter(|e| e.1)
                .map(|e| e.3)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(notes(1), vec![60]);
        assert_eq!(notes(2), vec![64, 67]);
        assert_eq!(notes(3), vec![70]);
    }

    #[test]
    fn a_repeated_note_releases_before_it_retriggers() {
        // Same chord in adjacent bars: at the bar line the off must precede
        // the on, or the new note is cancelled by the old one's release.
        let score = score_with(vec![
            chord(ScaleDegree::I, None),
            chord(ScaleDegree::I, None),
        ]);
        let parsed = single(&score);

        let mut pos = 0usize;
        let body = &parsed.tracks[0];
        let mut tick = 0u64;
        let mut at_bar_line: Vec<bool> = Vec::new(); // true == note-on
        let mut running = 0u8;
        while pos < body.len() {
            tick += read_vlq(body, &mut pos) as u64;
            let status = body[pos];
            if status == 0xFF {
                pos += 1;
                let kind = body[pos];
                pos += 1;
                let len = read_vlq(body, &mut pos) as usize;
                if kind == 0x2F {
                    break;
                }
                pos += len;
                continue;
            }
            if status & 0x80 != 0 {
                running = status;
                pos += 1;
            }
            let kind = running & 0xF0;
            let note = body[pos];
            let velocity = body[pos + 1];
            pos += 2;
            if tick == midi::BAR_TICKS && note == 60 {
                at_bar_line.push(matches!(kind, 0x90) && velocity > 0);
            }
        }

        // The C in bar two starts only after the C in bar one has stopped.
        assert_eq!(at_bar_line, vec![false, true]);
    }

    // ---- meta ----

    #[test]
    fn tempo_matches_the_bpm() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let parsed = single(&score);
        let tempo = meta_events(&parsed.tracks[0])
            .into_iter()
            .find(|(kind, _)| *kind == 0x51)
            .map(|(_, data)| data)
            .expect("tempo meta event");
        let micros = u32::from_be_bytes([0, tempo[0], tempo[1], tempo[2]]);
        assert_eq!(micros, 500_000); // 120 BPM
    }

    #[test]
    fn a_very_slow_tempo_saturates_instead_of_wrapping() {
        let score = midi::render_progression(
            &{
                let mut p = Progression::new();
                p.slots = vec![chord(ScaleDegree::I, None)];
                p
            },
            &Key::new(60, Scale::Major),
            1,
            1.0,
        );
        let parsed = single(&score);
        let tempo = meta_events(&parsed.tracks[0])
            .into_iter()
            .find(|(kind, _)| *kind == 0x51)
            .map(|(_, data)| data)
            .unwrap();
        assert_eq!(tempo, vec![0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn time_signature_is_four_four() {
        let parsed = single(&score_with(vec![chord(ScaleDegree::I, None)]));
        let sig = meta_events(&parsed.tracks[0])
            .into_iter()
            .find(|(kind, _)| *kind == 0x58)
            .map(|(_, data)| data)
            .unwrap();
        assert_eq!(sig, vec![4, 2, 24, 8]);
    }

    #[test]
    fn key_signature_is_spelled_by_pitch_class() {
        let c_major = Score {
            key: Key::new(60, Scale::Major),
            ..score_with(vec![chord(ScaleDegree::I, None)])
        };
        assert_eq!(key_signature(&c_major), (0, 0));

        let a_minor = Score {
            key: Key::new(57, Scale::Minor),
            ..score_with(vec![chord(ScaleDegree::I, None)])
        };
        assert_eq!(key_signature(&a_minor), (0, 1));

        let f_major = Score {
            key: Key::new(65, Scale::Major),
            ..score_with(vec![chord(ScaleDegree::I, None)])
        };
        assert_eq!(key_signature(&f_major), (-1, 0));
    }

    #[test]
    fn a_trailing_rest_extends_the_exported_length() {
        let parsed = single(&score_with(vec![chord(ScaleDegree::I, None), Slot::Rest]));
        // The last (and only) note-off is in bar one; the end-of-track delta
        // must still carry the loop out to the end of bar two.
        let found = events(&parsed.tracks[0]);
        let last = found.iter().map(|e| e.0).max().unwrap();
        assert_eq!(last, midi::BAR_TICKS);

        // Walk to the end-of-track and check the accumulated tick.
        let body = &parsed.tracks[0];
        let mut pos = 0usize;
        let mut tick = 0u64;
        while pos < body.len() {
            tick += read_vlq(body, &mut pos) as u64;
            let status = body[pos];
            if status == 0xFF && body[pos + 1] == 0x2F {
                break;
            }
            if status == 0xFF {
                pos += 2;
                let len = read_vlq(body, &mut pos) as usize;
                pos += len;
            } else {
                if status & 0x80 != 0 {
                    pos += 1;
                }
                pos += 2;
            }
        }
        assert_eq!(tick, 2 * midi::BAR_TICKS);
    }

    #[test]
    fn an_empty_score_still_writes_a_valid_file() {
        let parsed = single(&score_with(vec![]));
        assert_eq!(parsed.tracks.len(), 1);
        assert!(events(&parsed.tracks[0]).is_empty());
    }

    #[test]
    fn a_voice_with_note_data_round_trips_through_the_reader() {
        let score = score_with(vec![
            chord(ScaleDegree::I, None),
            Slot::Rest,
            chord(ScaleDegree::V, Some(Transformation::Dom7)),
        ]);
        let parsed = single(&score);

        let mut ons: Vec<(u64, u8)> = events(&parsed.tracks[0])
            .into_iter()
            .filter(|e| e.1)
            .map(|e| (e.0, e.3))
            .collect();
        ons.sort_unstable();
        assert_eq!(
            ons,
            vec![(0, 60), (0, 64), (0, 67), (2 * midi::BAR_TICKS, 67), (2 * midi::BAR_TICKS, 71), (2 * midi::BAR_TICKS, 74), (2 * midi::BAR_TICKS, 77)]
        );
    }

    // ---- embedded project document ----

    #[test]
    fn a_project_payload_survives_a_write_read_round_trip() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let payload = b"version = 1\nthis is the document\n".to_vec();
        let bytes = write(
            &score,
            &SmfOptions::single("Progression").with_project(payload.clone()),
        );

        assert_eq!(read_project(&bytes).unwrap(), Some(payload));
    }

    #[test]
    fn a_file_written_without_a_project_reports_none() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let bytes = write(&score, &SmfOptions::single("Progression"));
        assert_eq!(read_project(&bytes).unwrap(), None);
    }

    #[test]
    fn the_payload_is_found_in_a_per_layer_file_too() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let payload = b"version = 1\n".to_vec();
        let bytes = write(
            &score,
            &SmfOptions {
                layout: TrackLayout::PerLayer {
                    channels: ChannelMap::default(),
                },
                track_name: "Progression".to_string(),
                project: Some(payload.clone()),
            },
        );
        // It lives in the conductor track, which is the first one.
        assert_eq!(read_project(&bytes).unwrap(), Some(payload));
    }

    #[test]
    fn the_payload_is_still_hidden_from_note_parsers() {
        // The embedding must not disturb the note events a DAW reads.
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let with = write(
            &score,
            &SmfOptions::single("Progression").with_project(b"version = 1\n".to_vec()),
        );
        let without = write(&score, &SmfOptions::single("Progression"));

        let notes = |bytes: &[u8]| -> Vec<Event> {
            let parsed = parse(bytes);
            events(&parsed.tracks[0])
        };
        assert_eq!(notes(&with), notes(&without));
    }

    #[test]
    fn a_non_midi_file_is_an_error_not_a_silent_none() {
        assert!(matches!(read_project(b"short"), Err(SmfError::TooShort)));
        assert!(matches!(
            read_project(b"not a midi file at all"),
            Err(SmfError::NotMidi)
        ));
        let mut junk = vec![0u8; 32];
        junk[0..4].copy_from_slice(b"RIFF");
        assert!(matches!(read_project(&junk), Err(SmfError::NotMidi)));
    }

    #[test]
    fn a_truncated_file_is_reported() {
        let score = score_with(vec![chord(ScaleDegree::I, None)]);
        let bytes = write(&score, &SmfOptions::single("Progression"));
        let truncated = &bytes[..bytes.len() - 5];
        assert!(matches!(
            read_project(truncated),
            Err(SmfError::Truncated)
        ));
    }

    #[test]
    fn a_foreign_sequencer_specific_event_is_ignored() {
        // Some other tool's FF 7F data must not be mistaken for ours.
        assert!(strip_project_magic(&[0x01, b'C', b'T', b'P', b'1', 0x00]).is_none());
        assert!(strip_project_magic(&[MANUFACTURER_NON_COMMERCIAL, b'X', b'X', b'X', b'X']).is_none());
        assert!(strip_project_magic(&[MANUFACTURER_NON_COMMERCIAL]).is_none());
        assert!(strip_project_magic(&[]).is_none());
        assert_eq!(
            strip_project_magic(&[MANUFACTURER_NON_COMMERCIAL, b'C', b'T', b'P', b'1', 0x41]),
            Some(vec![0x41])
        );
    }
}
