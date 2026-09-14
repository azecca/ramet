//! Two ramet commands side by side: one waits for the other rather than both
//! writing the same env.json or picking the same ports.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ramet::commands::checkpoint;
use rustix::fs::{FlockOperation, flock};

use crate::support::{Fixture, PROJECT};

/// Holds the lock of `dir` for a moment, from another thread, as another ramet
/// process would. Returns the flag the thread raises just before releasing it.
fn held_elsewhere(dir: &std::path::Path) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let directory = File::open(dir).unwrap();
    flock(&directory, FlockOperation::LockExclusive).unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&released);
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::SeqCst);
        drop(directory);
    });
    (released, holder)
}

#[test]
fn a_command_waits_for_another_on_the_same_project() {
    let fx = Fixture::with_main_env();
    let (released, holder) = held_elsewhere(&fx.layout().project_dir(PROJECT));

    let args = checkpoint::Args {
        label: "c1".to_owned(),
        live: true,
        ..checkpoint::Args::default()
    };
    checkpoint::run(&fx.ctx(), &args).unwrap();

    assert!(
        released.load(Ordering::SeqCst),
        "ran while the lock was held"
    );
    holder.join().unwrap();
    assert!(
        fx.stderr()
            .contains("waiting for another ramet command on project demo"),
        "{}",
        fx.stderr()
    );
    assert!(fx.load("main").checkpoints.contains_key("c1"));
}

#[test]
fn choosing_ports_waits_for_another_command_choosing_its_own() {
    let fx = Fixture::with_main_env();
    let (released, holder) = held_elsewhere(fx.layout().root());

    crate::new::create(&fx, "feat-a", |_| {}).unwrap();

    assert!(released.load(Ordering::SeqCst), "chose ports while locked");
    holder.join().unwrap();
    assert!(
        fx.stderr().contains("to choose its ports"),
        "{}",
        fx.stderr()
    );
}
