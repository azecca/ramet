//! Regeneration of the compose configuration, run before every compose command.

use std::assert_matches;
use std::fs;
use std::time::{Duration, SystemTime};

use ramet::env::Env;
use ramet::error::Error;
use serde_json::json;

use crate::support::{Fixture, PROJECT, write_file, write_settings};

fn regenerate(fx: &Fixture, env: &mut Env, extra: &[&str]) {
    let extra: Vec<String> = extra.iter().map(|&p| p.to_owned()).collect();
    env.regenerate(&fx.ctx(), &extra).unwrap();
}

/// Profiles passed to each `docker compose config`.
fn resolved_profiles(fx: &Fixture) -> Vec<Vec<String>> {
    fx.runner
        .config_calls()
        .iter()
        .map(|cmd| {
            let argv = cmd.argv();
            argv.windows(2)
                .filter(|pair| pair[0] == "--profile")
                .map(|pair| pair[1].clone())
                .collect()
        })
        .collect()
}

/// The main env, whose worktree holds `settings` as its `.ramet.json`.
fn env_with_settings(fx: &Fixture, settings: &serde_json::Value) -> Env {
    write_settings(&fx.clone, settings);
    fx.save_env(fx.env("main", |env| env.worktree.clone_from(&fx.clone)))
}

// ---------------------------------------------------------------- profiles
// Without replaying the project's profiles, `checkpoint`, `new` and `restore`
// would regenerate the configuration without the profiled services, and the
// snapshot would miss their volumes.

#[test]
fn the_profiles_of_ramet_json_are_replayed() {
    let fx = Fixture::new();
    let mut env = env_with_settings(&fx, &json!({"compose": {"profiles": ["debug"]}}));
    regenerate(&fx, &mut env, &[]);
    assert_eq!(resolved_profiles(&fx), [["debug"]]);
}

#[test]
fn one_off_profiles_are_added_without_duplicates() {
    let fx = Fixture::new();
    let mut env = env_with_settings(&fx, &json!({"compose": {"profiles": ["debug"]}}));
    regenerate(&fx, &mut env, &["tools", "debug"]);
    assert_eq!(resolved_profiles(&fx), [["debug", "tools"]]);
}

#[test]
fn a_one_off_profile_is_not_recorded() {
    // `ramet compose run --profile tools migrate` must not attach `tools`.
    let fx = Fixture::new();
    let mut env = env_with_settings(&fx, &json!({"compose": {"profiles": ["debug"]}}));
    regenerate(&fx, &mut env, &["tools"]);
    regenerate(&fx, &mut env, &[]);
    assert_eq!(resolved_profiles(&fx)[1], ["debug"]);
}

#[test]
fn an_invalid_ramet_json_stops_before_compose() {
    let fx = Fixture::new();
    std::fs::write(fx.clone.join(".ramet.json"), "{\"compose\": [").unwrap();
    let mut env = fx.save_env(fx.env("main", |env| env.worktree.clone_from(&fx.clone)));
    let err = env.regenerate(&fx.ctx(), &[]).unwrap_err();
    assert_matches!(err, Error::InvalidSettings { .. });
    assert!(fx.runner.config_calls().is_empty());
}

// ----------------------------------------------------------- compose files

/// The main env, declaring `files`, which exist.
fn env_with_files(fx: &Fixture, files: &[&str]) -> Env {
    for file in files {
        write_file(&fx.clone, file, "services: {}\n");
    }
    env_with_settings(fx, &json!({"compose": {"files": files}}))
}

/// The `--project-directory` of the first `docker compose config`.
fn project_directory(fx: &Fixture) -> String {
    let argv = fx.runner.config_calls()[0].argv();
    let at = argv
        .iter()
        .position(|a| a == "--project-directory")
        .unwrap();
    argv[at + 1].clone()
}

#[test]
fn compose_files_go_through_compose_file() {
    // ramet never passes `-f` to resolve the configuration: chaining files
    // goes through compose's own mechanism.
    let fx = Fixture::new();
    let mut env = env_with_files(&fx, &["docker/compose/base.yml", "docker/compose/dev.yml"]);
    regenerate(&fx, &mut env, &[]);
    let cmd = &fx.runner.config_calls()[0];
    assert_eq!(
        cmd.env_var("COMPOSE_FILE").as_deref(),
        Some("docker/compose/base.yml:docker/compose/dev.yml")
    );
    assert_eq!(cmd.env_var("COMPOSE_PATH_SEPARATOR").as_deref(), Some(":"));
    assert!(!cmd.argv().contains(&"-f".to_owned()));
}

#[test]
fn nothing_is_set_without_declared_files() {
    let fx = Fixture::new();
    let mut env = env_with_files(&fx, &[]);
    regenerate(&fx, &mut env, &[]);
    assert_eq!(fx.runner.config_calls()[0].env_var("COMPOSE_FILE"), None);
    assert_eq!(project_directory(&fx), fx.clone.display().to_string());
}

#[test]
fn the_project_directory_is_the_one_of_the_first_file() {
    // As with `docker compose -f docker/compose/base.yml`: relative paths in
    // the files, and the `.env` compose reads, are those of that directory.
    let fx = Fixture::new();
    let mut env = env_with_files(&fx, &["docker/compose/base.yml", "docker/dev.yml"]);
    regenerate(&fx, &mut env, &[]);
    assert_eq!(
        project_directory(&fx),
        fx.clone.join("docker/compose").display().to_string()
    );
    assert_eq!(
        fx.runner.config_calls()[0].working_dir(),
        Some(fx.clone.as_path()),
        "`COMPOSE_FILE` stays relative to the worktree"
    );
}

#[test]
fn a_declared_file_that_does_not_exist_is_named() {
    let fx = Fixture::new();
    let mut env = env_with_settings(&fx, &json!({"compose": {"files": ["docker/base.yml"]}}));
    let err = env.regenerate(&fx.ctx(), &[]).unwrap_err();
    assert_matches!(err, Error::ComposeFileMissing { .. });
}

#[test]
fn compose_is_not_started_when_it_would_find_no_file() {
    // Testing the guard alone is not enough: it must be on the actual path.
    let fx = Fixture::new();
    let bare = fx.base.join("bare");
    fs::create_dir(&bare).unwrap();
    let err = fx.ctx().compose().resolve(&bare, &[], &[]).unwrap_err();
    assert_matches!(err, Error::ComposeNotDiscoverable { .. });
    assert!(
        fx.runner.config_calls().is_empty(),
        "compose must not run at all"
    );
}

#[test]
fn declared_files_make_the_guard_moot() {
    let fx = Fixture::new();
    let bare = fx.base.join("bare");
    fs::create_dir(&bare).unwrap();
    write_file(&bare, "a.yml", "services: {}\n");
    fx.ctx()
        .compose()
        .resolve(&bare, &[], &["a.yml".to_owned()])
        .unwrap();
}

#[test]
fn compose_file_from_the_shell_satisfies_the_guard() {
    let fx = Fixture::new();
    let bare = fx.base.join("bare");
    fs::create_dir(&bare).unwrap();
    fx.host
        .0
        .vars
        .borrow_mut()
        .insert("COMPOSE_FILE".into(), "x.yml".into());
    fx.ctx().compose().resolve(&bare, &[], &[]).unwrap();
}

#[test]
fn a_failing_config_reports_its_last_lines() {
    let fx = Fixture::new();
    fx.runner.fail("config");
    let err = fx.ctx().compose().resolve(&fx.clone, &[], &[]).unwrap_err();
    assert_matches!(err, Error::ComposeConfigFailed { .. });
    assert!(err.to_string().contains("simulated failure"));
}

// --------------------------------------------------------- generated file

fn example_config() -> serde_json::Value {
    json!({
        "name": "demo",
        "services": {"web": {"image": "nginx", "ports": [crate::support::port(80, 8080)]}},
        "volumes": {"pgdata": {"name": "demo_pgdata"}, "shared": {"external": true}},
        "networks": {"default": {"name": "demo_default"}},
    })
}

#[test]
fn writes_the_transformed_configuration() {
    let fx = Fixture::new();
    fx.runner.set_config(example_config());
    let mut env = fx.save_env(fx.env("feat-a", |env| {
        env.worktree = fx.clone.clone();
        env.parent = Some("main".into());
        env.ports.range = Some([30_000, 30_006].into());
        env.ports.map.insert("web:80".into(), 30_000);
    }));
    regenerate(&fx, &mut env, &[]);
    let generated: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(fx.layout().compose_file(PROJECT, "feat-a")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        generated["services"]["web"]["ports"][0]["published"],
        "30000"
    );
    let device = &generated["volumes"]["pgdata"]["driver_opts"]["device"];
    assert_eq!(
        device.as_str().unwrap(),
        fx.layout()
            .volumes_dir(PROJECT, "feat-a")
            .join("pgdata")
            .display()
            .to_string()
    );
    assert!(generated.get("name").is_none());
}

#[test]
fn creates_a_directory_per_managed_volume() {
    let fx = Fixture::new();
    fx.runner.set_config(example_config());
    let mut env = fx.save_env(fx.env("main", |env| env.worktree = fx.clone.clone()));
    regenerate(&fx, &mut env, &[]);
    let volumes = fx.layout().volumes_dir(PROJECT, "main");
    assert!(volumes.join("pgdata").is_dir());
    assert!(
        !volumes.join("shared").exists(),
        "external volumes are not ramet's"
    );
}

#[test]
fn an_unchanged_configuration_is_not_rewritten() {
    let fx = Fixture::new();
    let mut env = fx.save_env(fx.env("main", |env| env.worktree = fx.clone.clone()));
    regenerate(&fx, &mut env, &[]);
    let path = fx.layout().compose_file(PROJECT, "main");
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();

    regenerate(&fx, &mut env, &[]);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), old);

    fx.runner.set_config(json!({"services": {"db": {}}}));
    regenerate(&fx, &mut env, &[]);
    assert_ne!(fs::metadata(&path).unwrap().modified().unwrap(), old);
}

#[test]
fn no_temporary_file_is_left_behind() {
    let fx = Fixture::new();
    let mut env = fx.save_env(fx.env("main", |env| env.worktree = fx.clone.clone()));
    regenerate(&fx, &mut env, &[]);
    let leftovers: Vec<_> = fs::read_dir(fx.layout().env_dir(PROJECT, "main"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_new_published_port_is_recorded() {
    let fx = Fixture::new();
    fx.runner.set_config(example_config());
    let mut env = fx.save_env(fx.env("main", |env| env.worktree = fx.clone.clone()));
    regenerate(&fx, &mut env, &[]);
    assert_eq!(
        fx.load("main").ports.map["web:80"],
        8080,
        "main keeps the project's port"
    );
}
