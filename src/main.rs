//! The binary. Everything it does lives in the library, so that the same code
//! can be rendered, stressed and benchmarked from a test.

use std::io;

fn main() -> io::Result<()> {
    chord_tool::tui::run_interactive()
}
