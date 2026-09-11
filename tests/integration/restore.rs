//! `ramet restore`: rewinding data without rewinding metadata.

use ramet::commands::{Outcome, restore};
use ramet::env::Checkpoint;
use ramet::error::{Error, Result};
use ramet::ports::PortRange;
use std::assert_matches;

use crate::support::Fixture;

fn run(fx: &Fixture, label: &str, yes: bool, pre_restore: bool) -> Result<Outcome> {
    let args = restore::Args {
        label: label.to_owned(),
        yes,
        pre_restore,
    };
    restore::run(&fx.ctx(), &args)
}

/// A main env with a checkpoint `c1` recorded and on disk.
fn with_checkpoint() -> Fixture {
    let fx = Fixture::with_main_env();
    fx.checkpoint_dir("main", "c1");
    let mut main = fx.load("main");
    main.checkpoints.insert("c1".into(), Checkpoint::default());
    fx.save_env(main);
    fx
}

#[test]
fn refuses_an_unknown_checkpoint() {
    let fx = Fixture::with_main_env();
    let err = run(&fx, "never", true, false).unwrap_err();
    assert_matches!(err, Error::CheckpointNotFound { .. });
}

#[test]
fn deletes_the_subvolume_then_recreates_it_from_the_checkpoint() {
    let fx = with_checkpoint();
    assert_eq!(run(&fx, "c1", true, false).unwrap(), Outcome::Done);
    let log = fx.btrfs.0.borrow();
    assert_eq!(log.deleted, ["main"]);
    assert_eq!(log.snapshots, [("main@c1".into(), "main".into(), false)]);
}

#[test]
fn stops_then_restarts_the_stack() {
    let fx = with_checkpoint();
    run(&fx, "c1", true, false).unwrap();
    let verbs: Vec<String> = fx
        .runner
        .verbs_for("main")
        .into_iter()
        .filter(|verb| verb == "down" || verb == "up")
        .collect();
    assert_eq!(verbs, ["down", "up"]);
}

#[test]
fn keeps_the_checkpoints_and_ports_acquired_since() {
    // env.json lives in the subvolume: the snapshot would rewind it too.
    let fx = with_checkpoint();
    let mut main = fx.load("main");
    main.checkpoints.insert("c2".into(), Checkpoint::default());
    main.ports.range = Some(PortRange {
        first: 30_000,
        last: 30_006,
    });
    main.ports.map.insert("web:80".into(), 30_000);
    fx.save_env(main);
    run(&fx, "c1", true, false).unwrap();
    let restored = fx.load("main");
    assert_eq!(
        restored.checkpoints.keys().collect::<Vec<_>>(),
        ["c1", "c2"]
    );
    assert_eq!(
        restored.ports.range,
        Some(PortRange {
            first: 30_000,
            last: 30_006
        })
    );
}

#[test]
fn pre_restore_takes_a_safety_checkpoint() {
    let fx = with_checkpoint();
    run(&fx, "c1", true, true).unwrap();
    let labels: Vec<String> = fx.load("main").checkpoints.into_keys().collect();
    assert!(
        labels.iter().any(|label| label.starts_with("pre-restore-")),
        "{labels:?}"
    );
}

#[test]
fn without_a_terminal_it_requires_yes() {
    let fx = with_checkpoint();
    let err = run(&fx, "c1", false, false).unwrap_err();
    assert_matches!(err, Error::ConfirmationRequired { .. });
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn declining_changes_nothing() {
    let fx = with_checkpoint();
    fx.answer("n\n");
    assert_eq!(run(&fx, "c1", false, false).unwrap(), Outcome::Aborted);
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn offers_a_safety_checkpoint_in_a_terminal() {
    let fx = with_checkpoint();
    fx.answer("y\ny\n");
    run(&fx, "c1", false, false).unwrap();
    assert!(
        fx.stdout()
            .contains("take a \"pre-restore\" checkpoint first?")
    );
    assert!(
        fx.load("main")
            .checkpoints
            .keys()
            .any(|label| label.starts_with("pre-restore-"))
    );
}
