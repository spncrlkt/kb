//! MIDI export and import at the filesystem boundary.
//!
//! Everything that touches disk for MIDI lives here; the format itself is in
//! `crate::smf`, the musical model in `crate::midi`, and the embedded session
//! document in `crate::project` — all of which stay pure and I/O-free.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::midi::Score;
use crate::project::{self, ProjectError, Restored};
use crate::smf::{self, SmfError, SmfOptions};

/// Directory exports are written to, relative to the working directory.
///
/// Kept out of version control (see `.gitignore`) so a session's `.mid` files
/// never show up as untracked changes.
pub const PROGRESSIONS_DIR: &str = "progressions";

/// `base/progressions`.
pub fn export_dir_in(base: &Path) -> PathBuf {
    base.join(PROGRESSIONS_DIR)
}

/// The export directory for this run. Does not create it.
pub fn export_dir() -> PathBuf {
    export_dir_in(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// The export directory, created if it is missing.
///
/// The caller decides what to do about failure: the app falls back to the
/// working directory so a read-only checkout still starts and reports the
/// problem per export, rather than refusing to run.
pub fn ensure_export_dir() -> io::Result<PathBuf> {
    ensure_dir(&export_dir())
}

/// `create_dir_all`, returning the directory.
///
/// Split out so tests can exercise it against a temp base rather than the
/// process working directory.
fn ensure_dir(dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    Ok(dir.to_path_buf())
}

/// `progression-2026-09-23_18-03-45.mid`, from local wall-clock time.
///
/// `now` is a parameter rather than a call to `Local::now()` so the formatter
/// stays pure and testable without touching the clock.
pub fn export_filename(now: DateTime<Local>) -> String {
    format!("progression-{}.mid", now.format("%Y-%m-%d_%H-%M-%S"))
}

/// Serialise `score` and the session document, and write them into `dir`.
pub fn export(
    score: &Score,
    project: &[u8],
    dir: &Path,
    now: DateTime<Local>,
) -> io::Result<PathBuf> {
    write_score(score, project, &export_filename(now), dir)
}

/// Write `score` plus the embedded `project` document as `dir/filename`.
pub fn write_score(
    score: &Score,
    project: &[u8],
    filename: &str,
    dir: &Path,
) -> io::Result<PathBuf> {
    let opts = SmfOptions::single("Progression").with_project(project.to_vec());
    let bytes = smf::write(score, &opts);
    let path = dir.join(filename);
    fs::write(&path, bytes)?;
    Ok(path)
}

/// Why an import could not be completed.
#[derive(Debug)]
pub enum ImportError {
    /// The file could not be read.
    Io(io::Error),
    /// The file is not a usable MIDI file.
    Midi(SmfError),
    /// A valid MIDI file, but one chord-tool did not write.
    NotOurs,
    /// The embedded document could not be understood.
    Project(ProjectError),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(e) => write!(f, "could not read the file: {}", e),
            ImportError::Midi(e) => write!(f, "{}", e),
            ImportError::NotOurs => write!(
                f,
                // Kept short: this is drawn in a fixed-width panel row. The
                // legacy-export explanation lives in the README.
                "not a chord-tool file (no embedded progression)"
            ),
            ImportError::Project(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        ImportError::Io(e)
    }
}

impl From<SmfError> for ImportError {
    fn from(e: SmfError) -> Self {
        ImportError::Midi(e)
    }
}

impl From<ProjectError> for ImportError {
    fn from(e: ProjectError) -> Self {
        ImportError::Project(e)
    }
}

/// Read the session document out of an exported MIDI file.
///
/// Refuses a file that is not ours rather than guessing: the degrees and
/// transformations cannot be recovered from the notes, so a guess would
/// silently disagree with what was played.
pub fn import(path: &Path) -> Result<Restored, ImportError> {
    let bytes = fs::read(path)?;
    let payload = smf::read_project(&bytes)?.ok_or(ImportError::NotOurs)?;
    let document = project::decode(&payload)?;
    Ok(project::restore(&document)?)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::KeyPosition;
    use crate::midi;
    use crate::music::{Key, Scale, ScaleDegree, Transformation};
    use crate::progression::{Progression, ProgressionEntry, Registers, Slot};

    fn progression() -> Progression {
        let mut prog = Progression::new();
        prog.slots = vec![
            Slot::Chord(ProgressionEntry {
                registers: Registers {
                    left: Some([KeyPosition::LeftIndex].into()),
                    right: None,
                },
                ..ProgressionEntry::new(ScaleDegree::I, Some(Transformation::Diatonic7))
            }),
            Slot::Rest,
            Slot::Chord(ProgressionEntry::new(
                ScaleDegree::V,
                Some(Transformation::Dom7),
            )),
        ];
        prog
    }

    fn parts() -> (Score, Vec<u8>) {
        let key = Key::new(60, Scale::Major);
        let prog = progression();
        let score = midi::render_progression(&prog.slots, &key,
            120,
            0.5,
        );
        let document = project::encode(&prog, &crate::rhythm_store::RhythmStore::default(), key, 120, 0.5)
            .unwrap();
        (score, document)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("chord-tool-export-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn filename_is_a_local_timestamp() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 9, 23, 18, 3, 45).unwrap();
        assert_eq!(export_filename(now), "progression-2026-09-23_18-03-45.mid");
    }

    #[test]
    fn filename_zero_pads_every_field() {
        use chrono::TimeZone;
        let now = Local.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(export_filename(now), "progression-2026-01-02_03-04-05.mid");
    }

    #[test]
    fn exports_land_in_the_progressions_directory() {
        assert_eq!(
            export_dir_in(Path::new("/tmp/project")),
            Path::new("/tmp/project/progressions")
        );
        // The default is that directory under the working directory, so a run
        // from the repository root writes into the gitignored folder.
        assert!(export_dir().ends_with(PROGRESSIONS_DIR));
    }

    #[test]
    fn the_export_directory_is_created_on_demand() {
        // `ensure_export_dir` works on the process working directory, which a
        // test must not touch; this is the same call against a temp base.
        let base = temp_dir("ensure");
        let nested = export_dir_in(&base);
        assert!(!nested.exists());
        assert_eq!(ensure_dir(&nested).unwrap(), nested);
        assert!(nested.is_dir());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn write_score_creates_a_midi_file() {
        let dir = temp_dir("write");
        let (score, document) = parts();
        let path = write_score(&score, &document, "test.mid", &dir).unwrap();

        assert_eq!(path, dir.join("test.mid"));
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"MThd");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn export_uses_the_timestamped_name() {
        use chrono::TimeZone;
        let dir = temp_dir("timestamped");
        let (score, document) = parts();
        let now = Local.with_ymd_and_hms(2026, 9, 23, 18, 3, 45).unwrap();
        let path = export(&score, &document, &dir, now).unwrap();

        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "progression-2026-09-23_18-03-45.mid"
        );
        assert!(path.exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_score_reports_a_missing_directory() {
        let dir = std::env::temp_dir().join("chord-tool-export-does-not-exist");
        let _ = fs::remove_dir_all(&dir);
        let (score, document) = parts();
        assert!(write_score(&score, &document, "test.mid", &dir).is_err());
    }

    #[test]
    fn an_export_imports_back_to_the_same_session() {
        let dir = temp_dir("roundtrip");
        let (score, document) = parts();
        let path = write_score(&score, &document, "session.mid", &dir).unwrap();

        let restored = import(&path).unwrap();
        let original = progression();
        assert_eq!(restored.slots, original.slots);
        assert_eq!(restored.key, Key::new(60, Scale::Major));
        assert_eq!(restored.bpm, 120);
        assert_eq!(restored.note_length, 0.5);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = temp_dir("missing");
        let path = dir.join("nope.mid");
        assert!(matches!(import(&path), Err(ImportError::Io(_))));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_plain_midi_file_is_refused_as_not_ours() {
        let dir = temp_dir("foreign");
        let (score, _) = parts();
        // A file with notes but no embedded document, as an older export would
        // be, or as any other tool's file is.
        let bytes = smf::write(&score, &SmfOptions::single("Progression"));
        let path = dir.join("foreign.mid");
        fs::write(&path, bytes).unwrap();

        assert!(matches!(import(&path), Err(ImportError::NotOurs)));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_non_midi_file_is_refused_as_midi() {
        let dir = temp_dir("junk");
        let path = dir.join("junk.mid");
        fs::write(&path, b"definitely not midi").unwrap();
        assert!(matches!(import(&path), Err(ImportError::Midi(_))));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_not_ours_message_names_the_problem() {
        let message = ImportError::NotOurs.to_string();
        assert!(message.contains("not a chord-tool file"));
    }
}
