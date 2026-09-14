//! Read-only commands: `ls`, `log`, `path` and `prompt`.

use ramet::commands::{Outcome, log, ls, path, prompt};
use ramet::env::Checkpoint;
use ramet::error::Error;
use ramet::process::Output;
use ramet::storage::Usage;
use serde_json::Value;
use std::assert_matches;

use crate::support::{Fixture, PROJECT};

/// Standard output parsed as JSON: with `--json`, it must hold nothing else.
fn json_output(fx: &Fixture) -> Value {
    serde_json::from_str(&fx.stdout()).unwrap_or_else(|err| panic!("{err}: {}", fx.stdout()))
}

// ---------------------------------------------------------------------- ls

#[test]
fn ls_json_lists_every_env_and_the_current_one() {
    let fx = Fixture::with_main_env();
    fx.secondary_env("feat-a");
    assert_eq!(
        ls::run(&fx.ctx(), &ls::Args { json: true }).unwrap(),
        Outcome::Done
    );
    let listing = json_output(&fx);
    assert_eq!(listing["project"], PROJECT);
    assert_eq!(
        listing["current"], "main",
        "the current env is the one of the cwd"
    );
    let names: Vec<&str> = listing["envs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["feat-a", "main"]);
}

#[test]
fn ls_json_describes_each_env() {
    let fx = Fixture::with_main_env();
    let worktree = fx.add_worktree("feat-a");
    crate::support::write_settings(
        &worktree,
        &serde_json::json!({"compose": {"files": ["docker/base.yml"], "profiles": ["debug"]}}),
    );
    fx.save_env(fx.env("feat-a", |env| {
        env.parent = Some("main".into());
        env.worktree = worktree;
        env.ports.range = Some([30_000, 30_006].into());
        env.ports.map.insert("web:80".into(), 30_000);
        env.ports.map.insert("db:5432".into(), 30_001);
    }));
    ls::run(&fx.ctx(), &ls::Args { json: true }).unwrap();
    let listing = json_output(&fx);
    let feat = &listing["envs"][0];
    assert_eq!(feat["parent"], "main");
    assert_eq!(feat["branch"], "feat-a");
    assert_eq!(feat["worktree_exists"], true);
    assert_eq!(feat["compose_project"], "demo-feat-a");
    assert_eq!(feat["state"], "down");
    assert_eq!(feat["profiles"], serde_json::json!(["debug"]));
    assert_eq!(
        feat["compose_files"],
        serde_json::json!(["docker/base.yml"])
    );
    assert_eq!(feat["ports"]["range"], serde_json::json!([30_000, 30_006]));
    assert_eq!(
        feat["published"],
        serde_json::json!([
            {"service": "web", "container_port": 80, "host_port": 30_000},
            {"service": "db", "container_port": 5432, "host_port": 30_001},
        ])
    );
}

#[test]
fn ls_names_the_variable_of_a_named_port_where_it_is_set() {
    let fx = Fixture::with_main_env();
    let ports = serde_json::json!({"ports": {"web": "web:80"}});
    crate::support::write_settings(&fx.clone, &ports);
    fx.save_env(fx.env("main", |env| {
        env.worktree.clone_from(&fx.clone);
        env.ports.map.insert("web:80".into(), 8080);
    }));
    let worktree = fx.add_worktree("feat-a");
    crate::support::write_settings(&worktree, &ports);
    fx.save_env(fx.env("feat-a", |env| {
        env.parent = Some("main".into());
        env.worktree = worktree;
        env.ports.range = Some([30_000, 30_006].into());
        env.ports.map.insert("web:80".into(), 30_000);
    }));

    ls::run(&fx.ctx(), &ls::Args { json: false }).unwrap();
    let out = fx.stdout();
    assert!(
        out.contains("localhost:30000 → web:80  RAMET_PORT_WEB"),
        "{out}"
    );
    assert!(
        out.contains("localhost:8080 → web:80\n"),
        "unset where the env keeps the project's ports: {out}"
    );

    let fx_json = Fixture::with_main_env();
    let worktree = fx_json.add_worktree("feat-a");
    crate::support::write_settings(&worktree, &ports);
    fx_json.save_env(fx_json.env("feat-a", |env| {
        env.parent = Some("main".into());
        env.worktree = worktree;
        env.ports.range = Some([30_000, 30_006].into());
        env.ports.map.insert("web:80".into(), 30_000);
    }));
    ls::run(&fx_json.ctx(), &ls::Args { json: true }).unwrap();
    assert_eq!(
        json_output(&fx_json)["envs"][0]["published"],
        serde_json::json!([
            {"service": "web", "container_port": 80, "host_port": 30_000, "variables": ["RAMET_PORT_WEB"]},
        ])
    );
}

#[test]
fn ls_reports_a_missing_worktree() {
    let fx = Fixture::with_main_env();
    fx.save_env(fx.env("gone", |env| env.worktree = fx.base.join("vanished")));
    ls::run(&fx.ctx(), &ls::Args { json: true }).unwrap();
    let listing = json_output(&fx);
    let gone = listing["envs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "gone")
        .unwrap();
    assert_eq!(gone["worktree_exists"], false);
    assert!(gone["branch"].is_null());
}

#[test]
fn ls_says_which_env_the_star_marks() {
    let fx = Fixture::with_main_env();
    ls::run(&fx.ctx(), &ls::Args { json: false }).unwrap();
    let out = fx.stdout();
    assert!(out.contains(" * main  [down]"), "{out}");
    assert!(
        out.contains("env deduced from the working directory"),
        "{out}"
    );
}

#[test]
fn ls_says_when_no_env_matches_the_working_directory() {
    let fx = Fixture::with_main_env();
    let raw = fx.add_worktree("raw");
    ls::run(&fx.ctx_at(&raw), &ls::Args { json: false }).unwrap();
    assert!(fx.stdout().contains("no env matches the working directory"));
}

#[test]
fn ls_outside_a_project_suggests_init() {
    let fx = Fixture::new();
    let err = ls::run(&fx.ctx(), &ls::Args { json: false }).unwrap_err();
    assert_matches!(err, Error::NoProject { .. });
}

// --------------------------------------------------------------------- log

#[test]
fn log_json_describes_each_checkpoint() {
    let fx = Fixture::with_main_env();
    fx.btrfs.0.borrow_mut().usage = Usage {
        exclusive_bytes: Some(4096),
        ..Usage::default()
    };
    fx.checkpoint_dir("main", "c1");
    let mut main = fx.load("main");
    main.checkpoints.insert(
        "c1".into(),
        serde_json::from_value(serde_json::json!({
            "created_at": "2026-01-01T00:00:00+00:00", "head": "abc", "message": "m"}))
        .unwrap(),
    );
    fx.save_env(main);
    assert_eq!(
        log::run(&fx.ctx(), &log::Args { json: true }).unwrap(),
        Outcome::Done
    );
    let history = json_output(&fx);
    assert_eq!(history["env"], "main");
    let entry = &history["checkpoints"][0];
    assert_eq!(entry["label"], "c1");
    assert_eq!(entry["message"], "m");
    assert_eq!(entry["exists"], true);
    assert_eq!(entry["exclusive_bytes"], 4096);
}

#[test]
fn log_json_without_checkpoints() {
    let fx = Fixture::with_main_env();
    log::run(&fx.ctx(), &log::Args { json: true }).unwrap();
    assert_eq!(json_output(&fx)["checkpoints"], serde_json::json!([]));
}

#[test]
fn log_marks_partial_sizes_and_missing_subvolumes() {
    let fx = Fixture::with_main_env();
    fx.btrfs.0.borrow_mut().usage = Usage {
        exclusive_bytes: Some(2048),
        partial: true,
        ..Usage::default()
    };
    fx.checkpoint_dir("main", "c1");
    let mut main = fx.load("main");
    main.checkpoints.insert("c1".into(), Checkpoint::default());
    main.checkpoints
        .insert("lost".into(), Checkpoint::default());
    fx.save_env(main);
    log::run(&fx.ctx(), &log::Args { json: false }).unwrap();
    let out = fx.stdout();
    assert!(out.contains("used ≥ 2.0 KiB"), "{out}");
    assert!(out.contains("subvolume missing from disk"), "{out}");
    assert!(out.contains("lower bound"), "{out}");
}

// -------------------------------------------------------------------- path

#[test]
fn path_prints_the_current_worktree_alone() {
    let fx = Fixture::with_main_env();
    path::run(&fx.ctx(), &path::Args { name: None }).unwrap();
    assert_eq!(fx.stdout(), format!("{}\n", fx.clone.display()));
}

#[test]
fn path_of_a_named_env() {
    let fx = Fixture::with_main_env();
    let feat = fx.secondary_env("feat-a");
    path::run(
        &fx.ctx(),
        &path::Args {
            name: Some("feat-a".into()),
        },
    )
    .unwrap();
    assert_eq!(fx.stdout().trim(), feat.worktree.display().to_string());
}

#[test]
fn path_of_an_unknown_env_lists_the_known_ones() {
    let fx = Fixture::with_main_env();
    let err = path::run(
        &fx.ctx(),
        &path::Args {
            name: Some("ghost".into()),
        },
    )
    .unwrap_err();
    assert_matches!(err, Error::UnknownEnvironment { .. });
    assert!(err.hint().unwrap().contains("main"));
}

// ------------------------------------------------------------------ prompt

/// Makes the data root look mounted and writable, as `findmnt` would say.
fn mounted(fx: &Fixture) {
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        (argv[0] == "findmnt" && argv.contains(&"--target".to_owned()))
            .then(|| Output::success_with("btrfs\n"))
    });
}

#[test]
fn prompt_prints_project_and_env() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    prompt::run(&fx.ctx(), &prompt::Args { short: false }).unwrap();
    assert_eq!(fx.stdout(), "demo:main\n");
}

#[test]
fn prompt_short_prints_the_env_only() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    prompt::run(&fx.ctx(), &prompt::Args { short: true }).unwrap();
    assert_eq!(fx.stdout(), "main\n");
}

#[test]
fn prompt_is_silent_outside_an_env() {
    let fx = Fixture::new();
    mounted(&fx);
    assert_eq!(
        prompt::run(&fx.ctx(), &prompt::Args { short: false }).unwrap(),
        Outcome::Done
    );
    assert_eq!(fx.stdout(), "");
}

#[test]
fn prompt_is_silent_when_the_volume_is_not_mounted() {
    let fx = Fixture::with_main_env();
    assert_eq!(
        prompt::run(&fx.ctx(), &prompt::Args { short: false }).unwrap(),
        Outcome::Done
    );
    assert_eq!(fx.stdout(), "");
    let mounts = fx
        .runner
        .external_argvs()
        .into_iter()
        .filter(|argv| argv[0] == "mount")
        .count();
    assert_eq!(mounts, 0, "a shell prompt never mounts anything");
}
