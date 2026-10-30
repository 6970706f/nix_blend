//! Cleanup when the process is interrupted.

use std::sync::OnceLock;
use std::thread;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::emulate_default_handler;

use super::{atomic, process};

/// What else must happen before the process ends, like giving the terminal back.
static EXIT_HOOK: OnceLock<fn()> = OnceLock::new();

/// Registers `hook` to run when a signal ends the process.
pub fn on_exit(hook: fn()) {
    let _ = EXIT_HOOK.set(hook);
}

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
            if let Some(hook) = EXIT_HOOK.get() {
                hook();
            }
            let _ = emulate_default_handler(signal);
            std::process::exit(INTERRUPTED);
        }
    });
}
