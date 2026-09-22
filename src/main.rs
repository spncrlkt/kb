mod chime;
mod debug_log;
mod grammar;
mod keyboard;
mod music;
mod presets;
mod progression;
mod synth;
mod transport;
mod tui;

use std::io;

fn main() -> io::Result<()> {
    tui::run_interactive()
}
