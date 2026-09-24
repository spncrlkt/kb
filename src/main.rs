mod chime;
mod debug_log;
mod export;
mod grammar;
mod keyboard;
mod midi;
mod music;
mod presets;
mod progression;
mod project;
mod smf;
mod synth;
mod transport;
mod tui;

use std::io;

fn main() -> io::Result<()> {
    tui::run_interactive()
}
