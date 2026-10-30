//! Cleanup when the process is interrupted.

use std::thread;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::emulate_default_handler;

use super::{atomic, process};

/// Exit code of a process interrupted by a signal, if the default action fails.
const INTERRUPTED: i32 = 130;

/// Ends running tools and removes temporary files when interrupted, then lets the signal take its normal
/// course. A failure to install this only means that the next run does the cleanup.
pub fn install_cleanup() {
    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
        return;
    };
    thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            process::terminate_all();
            atomic::remove_registered();
            let _ = emulate_default_handler(signal);
            std::process::exit(INTERRUPTED);
        }
    });
}
