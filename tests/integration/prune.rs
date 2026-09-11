//! `ramet prune`: what lost its worktree is listed with its size, and deleted
//! once confirmed; what might still be wanted is left alone.

use std::assert_matches;
use std::fs;
use std::path::{Path, PathBuf};

use ramet::commands::{Outcome, prune};
use ramet::error::Error;
use ramet::storage::Usage;

use crate::support::{Fixture, PROJECT, git};

const MIB: u64 = 1 << 20;

fn run(fx: &Fixture, yes: bool) -> ramet::error::Result<Outcome> {
    prune::run(&fx.ctx(), &prune::Args { yes })
}

/// Writes an `env.json` for `name` of `project` under the data root,
/// recording `worktree`, as a snapshot or an env would hold it.
fn record(fx: &Fixture, project: &str, name: &str, worktree: &Path) -> PathBuf {
    let dir = fx.root.join(project).join(name);
    fs::create_dir_all(&dir).unwrap();
    let env = fx.env(name, |env| {
        project.clone_into(&mut env.project);
        worktree.clone_into(&mut env.worktree);
    });
    ramet::util::fs::write_json(&dir.join("env.json"), &env).unwrap();
    dir
}

/// A secondary env whose worktree was deleted by hand, with a checkpoint and
/// a generated configuration. Returns its former worktree.
fn orphaned_env(fx: &Fixture, name: &str) -> PathBuf {
    let worktree = fx.secondary_env(name).worktree;
    record(fx, PROJECT, &format!("{name}@c1"), &worktree);
    fs::write(fx.layout().compose_file(PROJECT, name), "{}").unwrap();
    fs::remove_dir_all(&worktree).unwrap();
    worktree
}

/// Every subvolume measures `own` bytes of its own.
fn sizes(fx: &Fixture, own: u64) {
    fx.btrfs.0.borrow_mut().usage = Usage {
        exclusive_bytes: Some(own),
        shared_bytes: Some(0),
        ..Usage::default()
    };
}

fn deleted(fx: &Fixture) -> Vec<String> {
    fx.btrfs.0.borrow().deleted.clone()
}

#[test]
fn nothing_to_prune_says_so() {
    let fx = Fixture::with_main_env();
    fx.secondary_env("feat-a");

    assert_eq!(run(&fx, false).unwrap(), Outcome::Done);
    assert!(fx.stdout().contains("nothing to prune"), "{}", fx.stdout());
    assert!(deleted(&fx).is_empty());
}

#[test]
fn deletes_an_env_whose_worktree_is_gone_and_its_checkpoints() {
    let fx = Fixture::with_main_env();
    sizes(&fx, MIB);
    let gone = orphaned_env(&fx, "feat-old");
    fx.secondary_env("feat-live");
    record(&fx, PROJECT, "main@c1", &fx.clone);
    fx.answer("y\n");

    assert_eq!(run(&fx, false).unwrap(), Outcome::Done);

    assert_eq!(deleted(&fx), ["feat-old", "feat-old@c1"]);
    assert_eq!(fx.env_names(), ["feat-live", "main"]);
    assert!(fx.root.join(PROJECT).join("main@c1").is_dir());
    let down = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "down")
        .expect("the stack is stopped");
    assert_eq!(down.project.as_deref(), Some("demo-feat-old"));
    assert_eq!(down.args, ["-v", "--remove-orphans"]);
    let worktrees = git(&fx.clone, &["worktree", "list"]);
    assert!(
        !worktrees.contains(gone.to_str().unwrap()),
        "git forgets the worktree: {worktrees}"
    );
    let out = fx.stdout();
    assert!(out.contains("demo/feat-old"), "{out}");
    assert!(out.contains("worktree gone"), "{out}");
    assert!(
        out.contains(gone.to_str().unwrap()),
        "the path is shown: {out}"
    );
    assert!(out.contains("at least 2.0 MiB to free"), "{out}");
    assert!(out.contains("2 subvolume(s) deleted"), "{out}");
}

#[test]
fn deletes_a_whole_project_whose_repository_is_gone() {
    let fx = Fixture::with_main_env();
    let vanished = fx.base.join("vanished");
    record(&fx, "other", "main", &vanished);
    record(&fx, "other", "main@c1", &vanished);
    fx.btrfs.0.borrow_mut().usage_of.insert(
        "other".to_owned(),
        Usage {
            exclusive_bytes: Some(30 * MIB),
            shared_bytes: Some(10 * MIB),
            partial: true,
            ..Usage::default()
        },
    );
    fx.answer("y\n");

    assert_eq!(run(&fx, false).unwrap(), Outcome::Done);
    assert_eq!(deleted(&fx), ["main", "main@c1"]);
    assert!(
        !fx.root.join("other").exists(),
        "the project directory goes"
    );
    assert!(
        fx.root.join(PROJECT).join("main").is_dir(),
        "{PROJECT} stays"
    );
    let out = fx.stdout();
    assert!(out.contains("project other"), "{out}");
    assert!(
        out.contains("≥ 40.0 MiB"),
        "shared bytes counted once: {out}"
    );
    assert!(out.contains("main, main@c1"), "{out}");
}

#[test]
fn deletes_leftovers_of_an_interrupted_init_and_empty_projects() {
    let fx = Fixture::with_main_env();
    fs::create_dir_all(fx.root.join(PROJECT).join("leftover")).unwrap();
    fs::create_dir_all(fx.root.join("empty")).unwrap();
    fx.answer("y\n");

    assert_eq!(run(&fx, false).unwrap(), Outcome::Done);
    assert_eq!(deleted(&fx), ["leftover"]);
    assert!(!fx.root.join("empty").exists());
    let out = fx.stdout();
    assert!(out.contains("no env.json"), "{out}");
    assert!(out.contains("empty directory"), "{out}");
}

#[test]
fn leaves_alone_what_might_still_be_wanted() {
    let fx = Fixture::with_main_env();
    // An env.json that cannot be read.
    let broken = fx.root.join(PROJECT).join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("env.json"), "{").unwrap();
    // A checkpoint whose env vanished while its worktree is still there: the
    // trace of an interrupted `restore`, the data to recover.
    record(&fx, PROJECT, "lost@c1", &fx.clone);

    assert_eq!(run(&fx, false).unwrap(), Outcome::Done);
    assert!(fx.stdout().contains("nothing to prune"), "{}", fx.stdout());
    assert!(broken.is_dir());
}

#[test]
fn declining_deletes_nothing() {
    let fx = Fixture::with_main_env();
    orphaned_env(&fx, "feat-old");
    fx.answer("n\n");

    assert_eq!(run(&fx, false).unwrap(), Outcome::Aborted);
    assert!(deleted(&fx).is_empty());
    assert!(
        !fx.runner
            .compose_calls()
            .iter()
            .any(|call| call.verb == "down"),
        "no stack is stopped either"
    );
}

#[test]
fn without_a_terminal_it_takes_yes() {
    let fx = Fixture::with_main_env();
    orphaned_env(&fx, "feat-old");

    assert_matches!(run(&fx, false), Err(Error::ConfirmationRequired { .. }));
    assert!(deleted(&fx).is_empty());
    assert!(fx.stdout().contains("demo/feat-old"), "listed all the same");

    assert_eq!(run(&fx, true).unwrap(), Outcome::Done);
    assert_eq!(deleted(&fx), ["feat-old", "feat-old@c1"]);
}

#[test]
fn announces_a_running_stack_before_stopping_it() {
    let fx = Fixture::with_main_env();
    orphaned_env(&fx, "feat-old");
    fx.runner
        .set_containers(r#"[{"Service": "db", "State": "running"}]"#);

    assert_matches!(run(&fx, false), Err(Error::ConfirmationRequired { .. }));
    assert!(
        fx.stdout()
            .contains("the stack of feat-old runs: it will be stopped"),
        "{}",
        fx.stdout()
    );
}

#[test]
fn a_project_directory_holding_something_else_stays() {
    let fx = Fixture::with_main_env();
    record(&fx, "other", "main", &fx.base.join("vanished"));
    fs::write(fx.root.join("other").join("notes.txt"), "mine").unwrap();

    assert_eq!(run(&fx, true).unwrap(), Outcome::Done);
    assert_eq!(deleted(&fx), ["main"]);
    assert!(fx.root.join("other").join("notes.txt").is_file());
    assert!(
        fx.stdout().contains("could not be removed"),
        "{}",
        fx.stdout()
    );
}
