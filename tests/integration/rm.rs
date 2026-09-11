//! `ramet rm`: refusals and removal order.

use ramet::commands::{Outcome, rm};
use ramet::error::{Error, Result};
use std::assert_matches;

use crate::support::{Fixture, PROJECT};

fn remove(fx: &Fixture, name: &str, keep_checkpoints: bool, yes: bool) -> Result<Outcome> {
    let args = rm::Args {
        name: name.to_owned(),
        keep_checkpoints,
        yes,
    };
    rm::run(&fx.ctx(), &args)
}

#[test]
fn refuses_an_unknown_env() {
    let fx = Fixture::with_main_env();
    let err = remove(&fx, "ghost", false, true).unwrap_err();
    assert_matches!(err, Error::UnknownEnvironment { .. });
    assert!(err.hint().unwrap().contains("main"));
}

#[test]
fn refuses_the_env_of_the_main_clone_whatever_its_name() {
    // Recognized by its missing parent, not by its name.
    let fx = Fixture::with_main_env();
    fx.save_env(fx.env("root", |env| env.parent = None));
    let err = remove(&fx, "root", false, true).unwrap_err();
    assert_matches!(err, Error::PrimaryEnvironment { .. });
}

#[test]
fn refuses_the_current_env() {
    let fx = Fixture::with_main_env();
    let mut main = fx.load("main");
    main.parent = Some("elsewhere".into());
    fx.save_env(main);
    let err = remove(&fx, "main", false, true).unwrap_err();
    assert_matches!(err, Error::CurrentEnvironment { .. });
    assert!(
        err.hint()
            .unwrap()
            .contains(&fx.clone.display().to_string())
    );
}

#[test]
fn removes_the_stack_the_worktree_and_the_subvolume() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    std::fs::write(fx.layout().compose_file(PROJECT, "feat-a"), "{}").unwrap();
    assert_eq!(remove(&fx, "feat-a", false, true).unwrap(), Outcome::Done);
    assert_eq!(fx.btrfs.0.borrow().deleted, ["feat-a"]);
    assert!(!feat.worktree.exists());
    let down = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "down")
        .unwrap();
    assert!(
        down.args.contains(&"-v".to_owned()),
        "docker volume objects go too"
    );
    let worktrees = crate::support::git(&fx.clone, &["worktree", "list"]);
    assert!(!worktrees.contains("feat-a"), "{worktrees}");
}

#[test]
fn checkpoints_go_before_their_env() {
    let fx = Fixture::with_main_env();
    fx.secondary_env("feat-a");
    fx.checkpoint_dir("feat-a", "c1");
    fx.checkpoint_dir("feat-a", "c2");
    remove(&fx, "feat-a", false, true).unwrap();
    assert_eq!(
        fx.btrfs.0.borrow().deleted,
        ["feat-a@c1", "feat-a@c2", "feat-a"]
    );
}

#[test]
fn keep_checkpoints_keeps_them() {
    let fx = Fixture::with_main_env();
    fx.secondary_env("feat-a");
    fx.checkpoint_dir("feat-a", "c1");
    remove(&fx, "feat-a", true, true).unwrap();
    assert_eq!(fx.btrfs.0.borrow().deleted, ["feat-a"]);
}

#[test]
fn declining_changes_nothing() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    fx.answer("n\n");
    assert_eq!(
        remove(&fx, "feat-a", false, false).unwrap(),
        Outcome::Aborted
    );
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
    assert!(feat.worktree.exists());
}

#[test]
fn a_worktree_deleted_by_hand_does_not_block_the_removal() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    std::fs::remove_dir_all(&feat.worktree).unwrap();
    remove(&fx, "feat-a", false, true).unwrap();
    assert_eq!(fx.btrfs.0.borrow().deleted, ["feat-a"]);
}

#[test]
fn every_path_comes_from_where_the_env_lies() {
    // A hand-edited or copied env.json naming another project must not point
    // the removal at that project's env.
    let fx = Fixture::with_main_env();
    let mut stale = fx.secondary_env("feat-a");
    stale.project = "other".to_owned();
    let layout = fx.layout();
    ramet::util::fs::write_json(&layout.env_dir(PROJECT, "feat-a").join("env.json"), &stale)
        .unwrap();
    std::fs::write(layout.compose_file(PROJECT, "feat-a"), "{}").unwrap();
    let foreign = layout.env_dir("other", "feat-a");
    std::fs::create_dir_all(&foreign).unwrap();

    remove(&fx, "feat-a", false, true).unwrap();

    assert!(foreign.exists(), "the other project's env was deleted");
    assert!(!layout.env_dir(PROJECT, "feat-a").exists());
    let down = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "down")
        .unwrap();
    assert_eq!(down.project.as_deref(), Some("demo-feat-a"));
}
