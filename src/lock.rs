//! Keeping two ramet commands from changing the same data at once.
//!
//! A human in a terminal and an agent in another, or two agents, may run
//! ramet side by side. Two commands writing the same `env.json` would lose
//! one's changes, and two envs created at once could pick the same ports. A
//! command that changes a project holds its lock, taken on the project's
//! directory; choosing a port block holds the lock of the data root, whose
//! blocks are shared by every project, for no longer than that.
//!
//! The locks are `flock` locks on those directories: nothing to create or
//! clean up, and a process that dies releases them. A command that finds a
//! lock held waits for it, saying so. Always the project first, then the data
//! root: in that order, two commands never wait on each other.

use std::fs::File;
use std::io;
use std::path::Path;

use rustix::fs::{FlockOperation, flock};

use crate::context::Context;
use crate::error::{Error, Result};

/// A lock held until dropped.
#[must_use = "the lock is released as soon as it is dropped"]
#[derive(Debug)]
pub struct Lock {
    _directory: File,
}

/// Holds the lock of the project `project`, waiting for whoever holds it.
pub fn project(ctx: &Context, project: &str) -> Result<Lock> {
    let dir = ctx.layout().project_dir(project);
    hold(&dir, || {
        let ui = ctx.ui();
        ui.err(ui.style().dim(format!(
            "waiting for another ramet command on project {project}…"
        )));
    })
}

/// Holds the lock on the port blocks of every project, waiting for whoever
/// holds it.
pub fn ports(ctx: &Context) -> Result<Lock> {
    hold(ctx.layout().root(), || {
        let ui = ctx.ui();
        ui.err(
            ui.style()
                .dim("waiting for another ramet command to choose its ports…"),
        );
    })
}

/// Holds the exclusive lock of the directory `dir`, calling `waiting` first
/// when someone else holds it.
fn hold(dir: &Path, waiting: impl FnOnce()) -> Result<Lock> {
    let io = |source: io::Error| Error::io(dir, source);
    let directory = File::open(dir).map_err(io)?;
    match flock(&directory, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => {
            waiting();
            flock(&directory, FlockOperation::LockExclusive).map_err(|errno| io(errno.into()))?;
        }
        Err(errno) => return Err(io(errno.into())),
    }
    Ok(Lock {
        _directory: directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    #[test]
    fn a_second_holder_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let first = hold(dir.path(), || panic!("nothing held yet")).unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let acquired = Arc::new(AtomicBool::new(false));
        let path = dir.path().to_owned();
        let flag = Arc::clone(&acquired);
        let second = std::thread::spawn(move || {
            let lock = hold(&path, || waiting_tx.send(()).unwrap()).unwrap();
            flag.store(true, Ordering::SeqCst);
            drop(lock);
        });

        waiting_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("told it waits");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!acquired.load(Ordering::SeqCst), "got in while held");
        drop(first);
        second.join().unwrap();
        assert!(acquired.load(Ordering::SeqCst));
    }

    #[test]
    fn a_lock_is_free_again_once_dropped() {
        let dir = tempfile::tempdir().unwrap();
        drop(hold(dir.path(), || panic!("free")).unwrap());
        drop(hold(dir.path(), || panic!("free again")).unwrap());
    }
}
