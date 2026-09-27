//! chord-tool: the chord grammar, the synth, the effects and the panels.
//!
//! A library as well as a binary, for one reason: the audio path has to be
//! renderable from a test. The engine is driven by the same code the audio
//! device drives, and `cargo bench` and `tests/` can only reach it if the
//! modules are a library rather than the private parts of an executable.
//!
//! `src/main.rs` is a two-line binary that calls [`tui::run_interactive`].
//! `PERFORMANCE.md` beside this file is how the audio is tested, stressed and
//! measured.

#![forbid(unsafe_code)]

pub mod analyzer;
pub mod arrangement;
pub mod debug_log;
pub mod ensemble;
pub mod eq;
pub mod export;
pub mod fx;
pub mod fx_dsp;
pub mod grammar;
pub mod history;
pub mod instrument;
pub mod keyboard;
pub mod midi;
pub mod music;
pub mod progression;
pub mod project;
pub mod rhythm;
pub mod rhythm_store;
pub mod settings;
pub mod smf;
pub mod synth;
pub mod timing;
pub mod transport;
pub mod tui;
pub mod voice;
pub mod wavetable;
