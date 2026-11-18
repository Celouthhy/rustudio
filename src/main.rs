//! rustudio's entry point. The program itself is the library (`src/lib.rs`),
//! so `tests/` and `examples/` can use `rustudio::…` instead of including
//! source files by path.
//!
//! This file stays a binary on purpose: the attributes below are the ones that
//! describe how the *executable* is presented to the OS.

// Launch as a GUI application on Windows without opening a console window.
// (Here, not in the library: it is the binary the OS starts.)
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    rustudio::run()
}
