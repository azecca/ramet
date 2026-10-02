//! `ramet checkpoint`: read-only snapshots with metadata.

use ramet::commands::{Outcome, checkpoint};
use ramet::env::Checkpoint;
use ramet::error::{Error, Result};
use std::assert_matches;

use crate::support::{Fixture, PROJECT};

fn take(fx: &Fixture, label: &str, customize: impl FnOnce(&mut checkpoint::Args)) -> Result<()> {
    let mut args = checkpoint::Args {
        label: label.to_owned(),
        ..checkpoint::Args::default()
    };
    customize(&mut args);
    checkpoint::run(&fx.ctx(), &args).map(drop)
}

fn delete(fx: &Fixture, label: &str, yes: bool) -> Result<Outcome> {
    let args = checkpoint::Args {
        label: label.to_owned(),
        delete: true,
        yes,
        ..checkpoint::Args::default()
    };
    checkpoint::run(&fx.ctx(), &args)
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
fn refuses_an_invalid_label() {
    let fx = Fixture::with_main_env();
    for label in ["a/b", "a@b", "-a"] {
        let err = take(&fx, label, |_| {}).unwrap_err();
        assert_matches!(err, Error::InvalidName { .. }, "{label}");
    }
}

#[test]
fn refuses_a_label_already_taken() {
    let fx = Fixture::with_main_env();
    let mut main = fx.load("main");
    main.checkpoints.insert("c1".into(), Checkpoint::default());
    fx.save_env(main);
    let err = take(&fx, "c1", |_| {}).unwrap_err();
    assert_matches!(err, Error::CheckpointExists { .. });
}

#[test]
fn refuses_a_label_whose_subvolume_exists() {
    let fx = Fixture::with_main_env();
    fx.checkpoint_dir("main", "c1");
    let err = take(&fx, "c1", |_| {}).unwrap_err();
    assert_matches!(err, Error::CheckpointExists { .. });
}

#[test]
fn snapshots_read_only_and_records_metadata() {
    let fx = Fixture::with_main_env();
    take(&fx, "c1", |args| {
        args.message = Some("before migration".into());
    })
    .unwrap();
    assert_eq!(
        fx.btrfs.0.borrow().snapshots,
        [("main".into(), "main@c1".into(), true)]
    );
    let meta = &fx.load("main").checkpoints["c1"];
    assert_eq!(meta.message.as_deref(), Some("before migration"));
    assert_eq!(meta.head.as_ref().map(String::len), Some(40));
    assert!(meta.created_at.is_some());
}

#[test]
fn freezes_a_running_stack() {
    let fx = Fixture::with_main_env();
    std::fs::write(
        fx.layout().compose_file(PROJECT, "main"),
        r#"{"services": {"web": {}}}"#,
    )
    .unwrap();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
    take(&fx, "c1", |_| {}).unwrap();
    let verbs = fx.runner.verbs_for("main");
    let frozen: Vec<&String> = verbs
        .iter()
        .filter(|v| *v == "pause" || *v == "unpause")
        .collect();
    assert_eq!(frozen, ["pause", "unpause"]);
}

#[test]
fn live_freezes_nothing() {
    let fx = Fixture::with_main_env();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
    take(&fx, "c1", |args| args.live = true).unwrap();
    assert!(fx.runner.verbs_for("main").is_empty(), "nothing is paused");
}

#[test]
fn a_failed_snapshot_still_thaws_and_records_nothing() {
    let fx = Fixture::with_main_env();
    std::fs::write(
        fx.layout().compose_file(PROJECT, "main"),
        r#"{"services": {"web": {}}}"#,
    )
    .unwrap();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
    fx.btrfs.0.borrow_mut().fail_snapshots = true;
    assert!(take(&fx, "c1", |_| {}).is_err());
    assert_eq!(
        fx.runner.verbs_for("main").last().map(String::as_str),
        Some("unpause")
    );
    assert!(fx.load("main").checkpoints.is_empty());
}

#[test]
fn delete_removes_the_subvolume_and_its_record() {
    let fx = with_checkpoint();
    fx.checkpoint_dir("main", "c2");
    let mut main = fx.load("main");
    main.checkpoints.insert("c2".into(), Checkpoint::default());
    fx.save_env(main);
    assert_eq!(delete(&fx, "c1", true).unwrap(), Outcome::Done);
    assert_eq!(fx.btrfs.0.borrow().deleted, ["main@c1"]);
    assert!(fx.btrfs.0.borrow().snapshots.is_empty());
    let labels: Vec<String> = fx.load("main").checkpoints.into_keys().collect();
    assert_eq!(labels, ["c2"], "the other checkpoint stays");
}

#[test]
fn delete_refuses_an_unknown_checkpoint() {
    let fx = with_checkpoint();
    let err = delete(&fx, "never", true).unwrap_err();
    assert_matches!(err, Error::CheckpointNotFound { .. });
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn delete_refuses_a_label_that_leaves_the_env() {
    // Joined into a path, this label reaches the checkpoint of another project.
    let fx = with_checkpoint();
    let other = fx.root.join("other").join("main@c1");
    std::fs::create_dir_all(&other).unwrap();
    let err = delete(&fx, "c1/../../other/main@c1", true).unwrap_err();
    assert_matches!(err, Error::InvalidName { kind: "label", .. });
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn delete_without_a_terminal_requires_yes() {
    let fx = with_checkpoint();
    let err = delete(&fx, "c1", false).unwrap_err();
    assert_matches!(err, Error::ConfirmationRequired { .. });
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
    assert!(fx.load("main").checkpoints.contains_key("c1"));
}

#[test]
fn declining_the_deletion_changes_nothing() {
    let fx = with_checkpoint();
    fx.answer("n\n");
    assert_eq!(delete(&fx, "c1", false).unwrap(), Outcome::Aborted);
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
    assert!(fx.load("main").checkpoints.contains_key("c1"));
}

#[test]
fn delete_forgets_a_checkpoint_whose_subvolume_is_gone() {
    let fx = Fixture::with_main_env();
    let mut main = fx.load("main");
    main.checkpoints.insert("c1".into(), Checkpoint::default());
    fx.save_env(main);
    assert_eq!(delete(&fx, "c1", true).unwrap(), Outcome::Done);
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
    assert!(fx.load("main").checkpoints.is_empty());
}

#[test]
fn delete_removes_a_subvolume_env_json_does_not_record() {
    let fx = Fixture::with_main_env();
    fx.checkpoint_dir("main", "c1");
    assert_eq!(delete(&fx, "c1", true).unwrap(), Outcome::Done);
    assert_eq!(fx.btrfs.0.borrow().deleted, ["main@c1"]);
}

#[test]
fn a_failed_deletion_keeps_the_record() {
    let fx = with_checkpoint();
    fx.btrfs.0.borrow_mut().fail_deletes = true;
    assert!(delete(&fx, "c1", true).is_err());
    assert!(fx.load("main").checkpoints.contains_key("c1"));
}
