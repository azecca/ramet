//! Finding envs on disk and deducing the current one from the working
//! directory, with real git worktrees.

use ramet::env::store;
use ramet::error::Error;
use std::assert_matches;

use crate::support::{Fixture, PROJECT, git};

#[test]
fn current_worktree_and_outside_a_repository() {
    let fx = Fixture::new();
    let ctx = fx.ctx();
    assert_eq!(
        ctx.git().current_worktree(&fx.clone),
        Some(fx.clone.clone())
    );
    let outside = fx.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    assert_eq!(ctx.git().current_worktree(&outside), None);
}

#[test]
fn branch_and_head() {
    let fx = Fixture::new();
    let ctx = fx.ctx();
    let git = ctx.git();
    assert_eq!(git.current_branch(&fx.clone).as_deref(), Some("main"));
    assert_eq!(git.head_sha(&fx.clone).map(|sha| sha.len()), Some(40));
    assert_eq!(
        git.current_branch(&fx.base.join("gone")),
        None,
        "a deleted worktree"
    );
}

#[test]
fn a_detached_head_has_no_branch() {
    let fx = Fixture::new();
    git(&fx.clone, &["checkout", "-q", "--detach"]);
    assert_eq!(fx.ctx().git().current_branch(&fx.clone), None);
}

#[test]
fn worktrees_list_the_main_clone_first() {
    let fx = Fixture::new();
    fx.add_worktree("feat-a");
    let ctx = fx.ctx();
    let trees = ctx.git().worktrees(&fx.clone);
    assert_eq!(trees[0].path, fx.clone);
    let branches: Vec<Option<&str>> = trees.iter().map(|tree| tree.branch.as_deref()).collect();
    assert_eq!(branches, [Some("main"), Some("feat-a")]);
}

#[test]
fn branch_exists() {
    let fx = Fixture::new();
    let ctx = fx.ctx();
    assert!(ctx.git().branch_exists(&fx.clone, "main"));
    assert!(!ctx.git().branch_exists(&fx.clone, "never-seen"));
}

#[test]
fn the_current_env_is_found_by_its_worktree() {
    let fx = Fixture::with_main_env();
    let env = store::current_env(&fx.ctx()).unwrap();
    assert_eq!((env.name.as_str(), env.project.as_str()), ("main", PROJECT));
}

#[test]
fn the_current_env_is_found_from_a_subdirectory() {
    let fx = Fixture::with_main_env();
    let sub = fx.clone.join("src/deep");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(store::current_env(&fx.ctx_at(&sub)).unwrap().name, "main");
}

#[test]
fn outside_a_repository_there_is_no_env() {
    let fx = Fixture::with_main_env();
    let outside = fx.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let err = store::current_env(&fx.ctx_at(&outside)).unwrap_err();
    assert_matches!(err, Error::NotInRepository { .. });
}

#[test]
fn a_repository_without_env_suggests_init() {
    let fx = Fixture::new();
    let err = store::current_env(&fx.ctx()).unwrap_err();
    assert_matches!(err, Error::NoEnvironment { .. });
    assert!(err.hint().unwrap().contains("ramet init"));
}

#[test]
fn a_worktree_created_outside_ramet_suggests_new() {
    let fx = Fixture::with_main_env();
    let raw = fx.add_worktree("raw");
    let err = store::current_env(&fx.ctx_at(&raw)).unwrap_err();
    assert_matches!(err, Error::UnmanagedWorktree { .. });
    assert!(err.hint().unwrap().contains("ramet new"));
}

#[test]
fn locate_finds_the_project_from_any_worktree() {
    let fx = Fixture::with_main_env();
    let raw = fx.add_worktree("raw");
    let location = store::locate(&fx.ctx_at(&raw));
    assert_eq!(location.project.as_deref(), Some(PROJECT));
    assert_eq!(location.worktree, Some(raw));
}

#[test]
fn main_clone_from_a_secondary_worktree() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    assert_eq!(store::main_clone(&fx.ctx(), &feat), fx.clone);
}

#[test]
fn checkpoints_are_never_mistaken_for_envs() {
    // A checkpoint holds a frozen copy of its env's env.json: listing it would
    // let that stale copy pass itself off as the env.
    let fx = Fixture::new();
    fx.save_env(fx.env("feat-a", |_| {}));
    fx.checkpoint_dir("feat-a", "c1");
    assert_eq!(fx.env_names(), ["feat-a"]);
    let files = store::env_files(&fx.layout());
    assert_eq!(files, [fx.layout().env_file(PROJECT, "feat-a")]);
}

#[test]
fn unreadable_env_files_are_skipped() {
    let fx = Fixture::new();
    fx.save_env(fx.env("good", |_| {}));
    let broken = fx.layout().env_dir(PROJECT, "broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("env.json"), "{not json").unwrap();
    assert_eq!(fx.env_names(), ["good"]);
}

#[test]
fn stack_state_reads_the_cached_configuration() {
    let fx = Fixture::with_main_env();
    let compose_file = fx.layout().compose_file(PROJECT, "main");
    std::fs::write(&compose_file, r#"{"services": {"web": {}, "db": {}}}"#).unwrap();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
    let state = store::stack_state(&fx.ctx(), PROJECT, "main");
    assert_eq!(
        state,
        ramet::compose::StackState::Partial,
        "db is declared but not running"
    );
}
