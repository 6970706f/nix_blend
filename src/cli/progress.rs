//! Progress of slow operations, on the standard error of a terminal.

use std::cell::Cell;
use std::io::{self, IsTerminal, Write};

use crate::application::ports::ProgressReporter;

/// Operations faster than this show nothing.
const QUIET_SECONDS: u64 = 2;
const CLEAR_LINE: &str = "\r\x1b[2K";

/// Shows the running operation and its elapsed time on one line. Without a terminal it
/// shows nothing, so that captured output stays clean.
#[derive(Debug)]
pub struct TerminalProgress {
    enabled: bool,
    drawn: Cell<bool>,
}

impl TerminalProgress {
    /// Creates the reporter for the current standard error.
    pub fn new() -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
            drawn: Cell::new(false),
        }
    }
}

impl ProgressReporter for TerminalProgress {
    fn update(&self, label: &str, seconds: u64) {
        if self.enabled && seconds >= QUIET_SECONDS {
            eprint!("{CLEAR_LINE}{label}... {seconds}s (Ctrl+C to cancel)");
            let _ = io::stderr().flush();
            self.drawn.set(true);
        }
    }

    fn finish(&self) {
        if self.drawn.replace(false) {
            eprint!("{CLEAR_LINE}");
            let _ = io::stderr().flush();
        }
    }
}
