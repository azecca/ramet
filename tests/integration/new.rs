//! `ramet new`: refusals, creation, inheritance, freezing and rollback.

use ramet::commands::{Outcome, new};
use ramet::error::{Error, Result};
use serde_json::json;
use std::assert_matches;

use crate::support::{Fixture, PROJECT, git, write_file, write_settings};

pub(crate) fn create(
    fx: &Fixture,
    name: &str,
    customize: impl FnOnce(&mut new::Args),
) -> Result<Outcome> {
    let mut args = new::Args {
        name: name.to_owned(),
        ..new::Args::default()
    };
    customize(&mut args);
    new::run(&fx.ctx(), &args)
}

#[test]
fn refuses_an_invalid_name() {
    let fx = Fixture::with_main_env();
    for name in ["feat/a", "feat@a", "-feat"] {
        let err = create(&fx, name, |_| {}).unwrap_err();
        assert_matches!(err, Error::InvalidName { .. }, "{name}");
    }
}

#[test]
fn refuses_an_existing_env() {
    let fx = Fixture::with_main_env();
    fx.save_env(fx.env("feat-a", |_| {}));
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::EnvironmentExists { .. });
}

#[test]
fn refuses_an_unknown_checkpoint() {
    let fx = Fixture::with_main_env();
    let err = create(&fx, "feat-a", |args| {
        args.from_checkpoint = Some("never".into());
    })
    .unwrap_err();
    assert_matches!(err, Error::CheckpointNotFound { .. });
}

/// Nothing was created: no snapshot, no worktree, not a line of output.
fn assert_untouched(fx: &Fixture) {
    assert!(fx.btrfs.0.borrow().snapshots.is_empty());
    assert!(!fx.base.join("app.wt").exists());
    assert_eq!(fx.stdout(), "");
    assert_eq!(fx.env_names(), ["main"]);
}

#[test]
fn refuses_before_anything_when_the_compose_file_is_not_committed() {
    // The file is there, but a worktree only holds what is committed.
    let fx = Fixture::with_main_env();
    crate::support::git(&fx.clone, &["rm", "-q", "--cached", "compose.yml"]);
    crate::support::git(&fx.clone, &["commit", "-qm", "untrack"]);
    assert!(fx.clone.join("compose.yml").is_file());

    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::ComposeFilesNotCommitted { .. });
    let message = crate::support::full_message(&err);
    assert!(
        message.contains("compose.yml is not committed: the new worktree would not have it"),
        "{message}"
    );
    assert!(
        message.contains("git add compose.yml && git commit"),
        "{message}"
    );
    assert_untouched(&fx);
}

#[test]
fn refuses_before_anything_in_a_repository_without_commit() {
    let fx = Fixture::new();
    let fresh = fx.base.join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    std::fs::write(fresh.join("compose.yml"), "services: {}\n").unwrap();
    crate::support::git(&fresh, &["init", "-q", "-b", "main"]);
    fx.save_env(fx.env("main", |env| env.worktree = fresh.clone()));

    let args = new::Args {
        name: "feat-a".to_owned(),
        ..new::Args::default()
    };
    let err = new::run(&fx.ctx_at(&fresh), &args).unwrap_err();
    assert_matches!(err, Error::NoCommit { .. });
    assert!(err.hint().unwrap().contains("git add -A && git commit"));
    assert!(fx.btrfs.0.borrow().snapshots.is_empty());
    assert!(!fx.base.join("fresh.wt").exists());
}

#[test]
fn refuses_an_existing_branch_that_lacks_the_compose_file() {
    let fx = Fixture::with_main_env();
    let git = |args: &[&str]| crate::support::git(&fx.clone, args);
    git(&["checkout", "-q", "--orphan", "bare"]);
    git(&["rm", "-rq", "--cached", "."]);
    git(&["commit", "-q", "--allow-empty", "-m", "bare"]);
    git(&["checkout", "-q", "-f", "main"]);

    let err = create(&fx, "feat-a", |args| args.branch = Some("bare".into())).unwrap_err();
    assert_matches!(
        err,
        Error::ComposeFilesNotCommitted {
            branch: Some(_),
            ..
        }
    );
    assert!(err.to_string().contains("not in branch \"bare\""), "{err}");
    assert_untouched(&fx);
}

#[test]
fn creates_the_worktree_the_subvolume_and_the_env_file() {
    let fx = Fixture::with_main_env();
    assert_eq!(create(&fx, "feat-a", |_| {}).unwrap(), Outcome::Done);
    assert!(fx.base.join("app.wt/feat-a").is_dir());
    assert_eq!(fx.env_names(), ["feat-a", "main"]);
    assert_eq!(
        fx.btrfs.0.borrow().snapshots,
        [("main".into(), "feat-a".into(), false)]
    );
    let env = fx.load("feat-a");
    assert_eq!(env.parent.as_deref(), Some("main"));
    assert!(env.ports.range.is_some());
    assert!(fx.stdout().contains("env \"feat-a\" ready."));
}

#[test]
fn starts_the_new_stack() {
    let fx = Fixture::with_main_env();
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(fx.runner.verbs_for("feat-a").contains(&"up".to_owned()));
}

#[test]
fn an_uncommitted_ramet_json_comes_along() {
    // A project that keeps `.ramet.json` out of its repository still runs
    // the same way in every env.
    let fx = Fixture::with_main_env();
    write_settings(
        &fx.clone,
        &json!({"compose": {"files": ["compose.yml"], "profiles": ["debug"]}}),
    );
    create(&fx, "feat-a", |_| {}).unwrap();
    let worktree = fx.base.join("app.wt/feat-a");
    assert!(worktree.join(".ramet.json").is_file());
    let config = fx
        .runner
        .config_calls()
        .into_iter()
        .rfind(|cmd| cmd.working_dir() == Some(worktree.as_path()))
        .expect("the new worktree is resolved");
    assert!(
        config
            .argv()
            .windows(2)
            .any(|pair| pair == ["--profile", "debug"])
    );
    assert_eq!(
        config.env_var("COMPOSE_FILE").as_deref(),
        Some("compose.yml")
    );
}

#[test]
fn a_committed_ramet_json_is_taken_from_the_branch() {
    // The branch checked out decides; the source's copy is not synced over it.
    let fx = Fixture::with_main_env();
    write_settings(&fx.clone, &json!({"compose": {"profiles": ["committed"]}}));
    git(&fx.clone, &["add", ".ramet.json"]);
    git(&fx.clone, &["commit", "-qm", "settings"]);
    write_settings(&fx.clone, &json!({"compose": {"profiles": ["local"]}}));
    create(&fx, "feat-a", |_| {}).unwrap();
    let copy = std::fs::read_to_string(fx.base.join("app.wt/feat-a/.ramet.json")).unwrap();
    assert!(copy.contains("committed"), "{copy}");
}

#[test]
fn refuses_a_branch_whose_declared_compose_files_are_not_committed() {
    let fx = Fixture::with_main_env();
    write_file(&fx.clone, "docker/base.yml", "services: {}\n");
    write_settings(
        &fx.clone,
        &json!({"compose": {"files": ["docker/base.yml"]}}),
    );
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::ComposeFilesNotCommitted { files, .. } if files == ["docker/base.yml"]);
    assert_eq!(
        fx.env_names(),
        ["main"],
        "refused before anything is created"
    );
}

#[test]
fn an_invalid_ramet_json_is_refused_before_anything_is_created() {
    let fx = Fixture::with_main_env();
    std::fs::write(fx.clone.join(".ramet.json"), r#"{"synk": []}"#).unwrap();
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::InvalidSettings { .. });
    assert!(fx.btrfs.0.borrow().snapshots.is_empty());
}

#[test]
fn a_clone_cut_short_names_its_own_worktree_never_the_source_s() {
    // The snapshot copies main's env.json, naming main's worktree. A run that
    // stops before cleaning up (a failure here, Ctrl-C in real life) must not
    // leave an env whose removal would take main's worktree.
    let fx = Fixture::with_main_env();
    fx.btrfs.0.borrow_mut().fail_deletes = true;
    // `main` is checked out in the main clone: `git worktree add` fails.
    let err = create(&fx, "feat-a", |args| args.branch = Some("main".into()));
    assert!(err.is_err());

    let leftover = fx.load("feat-a");
    assert_eq!(leftover.worktree, fx.base.join("app.wt/feat-a"));
    assert_ne!(leftover.worktree, fx.clone);
    assert_eq!(leftover.parent.as_deref(), Some("main"));
}

#[test]
fn checks_out_the_requested_branch() {
    let fx = Fixture::with_main_env();
    create(&fx, "feat-a", |args| args.branch = Some("other".into())).unwrap();
    let worktree = fx.base.join("app.wt/feat-a");
    assert_eq!(
        fx.ctx().git().current_branch(&worktree).as_deref(),
        Some("other")
    );
}

#[test]
fn empty_options_count_as_absent() {
    // `--branch "$BRANCH"` with `BRANCH` unset: the branch takes the env name.
    let fx = Fixture::with_main_env();
    create(&fx, "feat-a", |args| {
        args.branch = Some(String::new());
        args.from_checkpoint = Some(String::new());
    })
    .unwrap();
    let worktree = fx.base.join("app.wt/feat-a");
    assert_eq!(
        fx.ctx().git().current_branch(&worktree).as_deref(),
        Some("feat-a")
    );
}

#[test]
fn reuses_an_existing_branch() {
    let fx = Fixture::with_main_env();
    crate::support::git(&fx.clone, &["branch", "existing"]);
    create(&fx, "feat-a", |args| args.branch = Some("existing".into())).unwrap();
    let worktree = fx.base.join("app.wt/feat-a");
    assert_eq!(
        fx.ctx().git().current_branch(&worktree).as_deref(),
        Some("existing")
    );
}

#[test]
fn starts_from_a_checkpoint() {
    let fx = Fixture::with_main_env();
    fx.checkpoint_dir("main", "c1");
    create(&fx, "feat-a", |args| {
        args.from_checkpoint = Some("c1".into());
    })
    .unwrap();
    assert_eq!(
        fx.btrfs.0.borrow().snapshots,
        [("main@c1".into(), "feat-a".into(), false)]
    );
    assert!(fx.stdout().contains("from main@c1"));
}

#[test]
fn rolls_back_when_the_new_stack_fails_to_start() {
    let fx = Fixture::with_main_env();
    fx.runner.fail("up");
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::CommandFailed { .. });
    assert!(
        fx.btrfs.0.borrow().deleted.contains(&"feat-a".to_owned()),
        "the subvolume goes"
    );
    assert!(!fx.base.join("app.wt/feat-a").exists(), "the worktree goes");
    assert!(
        !fx.base.join("app.wt").exists(),
        "so does the directory of worktrees ramet created for it"
    );
    assert_eq!(fx.env_names(), ["main"], "no half-created env is left");
}

#[test]
fn a_rollback_keeps_the_directory_of_other_worktrees() {
    let fx = Fixture::with_main_env();
    let other = fx.add_worktree("other");
    fx.runner.fail("up");
    create(&fx, "feat-a", |_| {}).unwrap_err();
    assert!(!fx.base.join("app.wt/feat-a").exists());
    assert!(other.is_dir(), "another worktree lives there");
}

// ----------------------------------------------------------------- freezing
// The source stack is frozen, not stopped: `stop` cut connections for
// seconds, and an agent working there would start "fixing" sound code.

fn running(fx: &Fixture) {
    std::fs::write(
        fx.layout().compose_file(PROJECT, "main"),
        r#"{"services": {"web": {}}}"#,
    )
    .unwrap();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
}

#[test]
fn freezes_and_thaws_around_the_snapshot_instead_of_stopping() {
    let fx = Fixture::with_main_env();
    running(&fx);
    create(&fx, "feat-a", |_| {}).unwrap();
    let verbs: Vec<String> = fx
        .runner
        .verbs_for("main")
        .into_iter()
        .filter(|verb| verb != "ps" && verb != "config")
        .collect();
    assert_eq!(verbs, ["pause", "unpause"]);
}

#[test]
fn live_freezes_nothing() {
    let fx = Fixture::with_main_env();
    running(&fx);
    create(&fx, "feat-a", |args| args.live = true).unwrap();
    assert!(!fx.runner.verbs_for("main").contains(&"pause".to_owned()));
}

#[test]
fn a_stopped_stack_is_not_frozen() {
    let fx = Fixture::with_main_env();
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(!fx.runner.verbs_for("main").contains(&"pause".to_owned()));
}

#[test]
fn a_checkpoint_needs_no_freezing() {
    let fx = Fixture::with_main_env();
    running(&fx);
    fx.checkpoint_dir("main", "c1");
    create(&fx, "feat-a", |args| {
        args.from_checkpoint = Some("c1".into());
    })
    .unwrap();
    assert!(!fx.runner.verbs_for("main").contains(&"pause".to_owned()));
}

#[test]
fn asks_in_a_terminal_with_freezing_as_the_default() {
    let fx = Fixture::with_main_env();
    running(&fx);
    fx.answer("\n");
    create(&fx, "feat-a", |_| {}).unwrap();
    assert!(fx.stdout().contains("freeze? [Y/n]"));
    assert!(fx.runner.verbs_for("main").contains(&"pause".to_owned()));
}

#[test]
fn declining_the_freeze_still_creates_the_env() {
    let fx = Fixture::with_main_env();
    running(&fx);
    fx.answer("n\n");
    assert_eq!(create(&fx, "feat-a", |_| {}).unwrap(), Outcome::Done);
    assert!(!fx.runner.verbs_for("main").contains(&"pause".to_owned()));
    assert_eq!(fx.env_names(), ["feat-a", "main"]);
}

#[test]
fn the_source_is_thawed_even_when_the_snapshot_fails() {
    let fx = Fixture::with_main_env();
    running(&fx);
    fx.btrfs.0.borrow_mut().fail_snapshots = true;
    let err = create(&fx, "feat-a", |_| {}).unwrap_err();
    assert_matches!(err, Error::CommandFailed { .. });
    let verbs = fx.runner.verbs_for("main");
    assert!(verbs.contains(&"pause".to_owned()), "{verbs:?}");
    assert_eq!(
        verbs.last().map(String::as_str),
        Some("unpause"),
        "{verbs:?}"
    );
    assert!(!fx.base.join("app.wt/feat-a").exists());
}
