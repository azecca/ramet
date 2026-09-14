//! Ctrl-C, and the signals that end a process, while ramet must not stop.
//!
//! Ctrl-C ends ramet on the spot, like any program, and that is right almost
//! everywhere. Not while a stack is frozen, which would stay paused and hang
//! every client of that env without a word, nor while a restore swaps an
//! env's data. For as long as a [`Deferral`] is held, Ctrl-C is recorded
//! instead. The external command running at that moment still receives it
//! from the terminal and stops, so the command unwinds as on any failure,
//! thaws the stack, and ramet then exits as interrupted.
//!
//! `SIGTERM` and `SIGHUP` are deferred alike: an agent's tool killing a
//! command that runs too long, or a closed terminal, must not freeze a stack
//! for good either. `SIGKILL` cannot be caught: `ramet doctor` reports what it
//! leaves behind.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

/// State shared with the signal handler.
struct Flags {
    /// Whether Ctrl-C takes its default action and ends the process.
    immediate: Arc<AtomicBool>,
    /// Whether Ctrl-C was pressed while deferred.
    received: Arc<AtomicBool>,
}

/// Set once, by the first deferral; `None` when the handler could not be
/// installed, in which case Ctrl-C keeps its default action.
static FLAGS: OnceLock<Option<Flags>> = OnceLock::new();

fn install() -> Option<Flags> {
    let flags = Flags {
        immediate: Arc::new(AtomicBool::new(true)),
        received: Arc::new(AtomicBool::new(false)),
    };
    // Actions run in registration order: while `immediate` is set, the
    // default action ends the process before anything is recorded.
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        signal_hook::flag::register_conditional_default(signal, Arc::clone(&flags.immediate))
            .ok()?;
        signal_hook::flag::register(signal, Arc::clone(&flags.received)).ok()?;
    }
    Some(flags)
}

/// While held, Ctrl-C, `SIGTERM` and `SIGHUP` are recorded rather than ending
/// the process.
///
/// Deferrals do not nest: one stack is frozen at a time.
#[must_use = "Ctrl-C is only deferred while the guard is held"]
pub struct Deferral {
    flags: Option<&'static Flags>,
}

impl Deferral {
    /// Defers Ctrl-C until the guard is dropped.
    pub fn begin() -> Self {
        let flags = FLAGS.get_or_init(install).as_ref();
        if let Some(flags) = flags {
            flags.immediate.store(false, Ordering::SeqCst);
        }
        Self { flags }
    }
}

impl Drop for Deferral {
    fn drop(&mut self) {
        if let Some(flags) = self.flags {
            flags.immediate.store(true, Ordering::SeqCst);
        }
    }
}

/// Whether Ctrl-C was pressed, or the process asked to end, while deferred.
pub fn received() -> bool {
    FLAGS
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|flags| flags.received.load(Ordering::SeqCst))
}
