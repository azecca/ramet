//! Ctrl-C while the stack of an env is frozen.
//!
//! Ctrl-C ends ramet on the spot, like any program, and that is right almost
//! everywhere. Not while a stack is frozen: ending there would leave it
//! paused, hanging every client of that env without a word. For as long as a
//! [`Deferral`] is held, Ctrl-C is recorded instead. The external command
//! running at that moment still receives it from the terminal and stops, so
//! the command unwinds as on any failure, thaws the stack, and ramet then
//! exits as interrupted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use signal_hook::consts::SIGINT;

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
    signal_hook::flag::register_conditional_default(SIGINT, Arc::clone(&flags.immediate)).ok()?;
    signal_hook::flag::register(SIGINT, Arc::clone(&flags.received)).ok()?;
    Some(flags)
}

/// While held, Ctrl-C is recorded rather than ending the process.
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

/// Whether Ctrl-C was pressed while deferred.
pub fn received() -> bool {
    FLAGS
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|flags| flags.received.load(Ordering::SeqCst))
}
