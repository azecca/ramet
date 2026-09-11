//! `ramet deinit`: handing the project back, the counterpart of reversibility.

use ramet::commands::{Outcome, deinit};
use ramet::error::{Error, Result};
use std::assert_matches;

use crate::support::{Fixture, PROJECT};

fn run_in(fx: &Fixture, dir: &std::path::Path, yes: bool) -> Result<Outcome> {
    deinit::run(&fx.ctx_at(dir), &deinit::Args { yes })
}

#[test]
fn refuses_outside_a_project() {
    let fx = Fixture::new();
    let err = run_in(&fx, &fx.clone, true).unwrap_err();
    assert_matches!(err, Error::NoEnvironment { .. });
}

#[test]
fn refuses_while_secondary_envs_remain() {
    let fx = Fixture::with_main_env();
    fx.save_env(fx.env("feat-a", |env| env.parent = Some("main".into())));
    let err = run_in(&fx, &fx.clone, true).unwrap_err();
    assert_matches!(err, Error::EnvironmentsRemain { ref names } if names == &["feat-a"]);
    assert!(err.hint().unwrap().contains("ramet rm"));
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn refuses_from_a_secondary_env() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    let err = run_in(&fx, &feat.worktree, true).unwrap_err();
    assert_matches!(err, Error::DeinitOutsideMainClone { .. });
}

#[test]
fn declining_changes_nothing() {
    let fx = Fixture::with_main_env();
    fx.answer("n\n");
    assert_eq!(run_in(&fx, &fx.clone, false).unwrap(), Outcome::Aborted);
    assert!(fx.btrfs.0.borrow().deleted.is_empty());
}

#[test]
fn deletes_checkpoints_then_the_env_then_the_project_directory() {
    let fx = Fixture::with_main_env();
    fx.checkpoint_dir("main", "c1");
    fx.checkpoint_dir("main", "c2");
    assert_eq!(run_in(&fx, &fx.clone, true).unwrap(), Outcome::Done);
    assert_eq!(fx.btrfs.0.borrow().deleted, ["main@c1", "main@c2", "main"]);
    assert!(!fx.layout().project_dir(PROJECT).exists());
}

#[test]
fn stops_the_stack_and_removes_its_volume_objects() {
    let fx = Fixture::with_main_env();
    std::fs::write(fx.layout().compose_file(PROJECT, "main"), "{}").unwrap();
    run_in(&fx, &fx.clone, true).unwrap();
    let down = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "down")
        .unwrap();
    assert!(down.args.contains(&"-v".to_owned()));
}

#[test]
fn leaves_the_repository_alone() {
    let fx = Fixture::with_main_env();
    run_in(&fx, &fx.clone, true).unwrap();
    assert!(fx.clone.join(".git").is_dir());
    assert!(fx.clone.join("compose.yml").is_file());
    assert_eq!(
        crate::support::git(&fx.clone, &["status", "--porcelain"]),
        ""
    );
}

#[test]
fn something_unexpected_in_the_project_directory_is_reported() {
    let fx = Fixture::with_main_env();
    std::fs::write(fx.layout().project_dir(PROJECT).join("stray"), "").unwrap();
    let err = run_in(&fx, &fx.clone, true).unwrap_err();
    assert_matches!(err, Error::ProjectDirNotRemovable { .. });
}
