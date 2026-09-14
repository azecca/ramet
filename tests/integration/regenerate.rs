//! Regeneration of the compose configuration, run before every compose command.

use std::assert_matches;
use std::fs;
use std::time::{Duration, SystemTime};

use ramet::compose::Interpolation;
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
    let err = fx
        .ctx()
        .compose()
        .resolve(&bare, &[], &[], &Interpolation::default())
        .unwrap_err();
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
        .resolve(&bare, &[], &["a.yml".to_owned()], &Interpolation::default())
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
    fx.ctx()
        .compose()
        .resolve(&bare, &[], &[], &Interpolation::default())
        .unwrap();
}

#[test]
fn a_failing_config_reports_its_last_lines() {
    let fx = Fixture::new();
    fx.runner.fail("config");
    let err = fx
        .ctx()
        .compose()
        .resolve(&fx.clone, &[], &[], &Interpolation::default())
        .unwrap_err();
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

// ------------------------------------------------------------- named ports
// The developer writes `${RAMET_PORT_WEB}` where an address needs the env's
// port: compose interpolates it, ramet never touches the project's files.

/// `feat-a`, in the main clone's worktree declaring `ports`, with its own
/// block where `web:80` is on 30000.
fn env_with_named_ports(fx: &Fixture, ports: &serde_json::Value) -> Env {
    write_settings(&fx.clone, &json!({ "ports": ports }));
    fx.runner.set_config(example_config());
    fx.save_env(fx.env("feat-a", |env| {
        env.worktree.clone_from(&fx.clone);
        env.parent = Some("main".into());
        env.ports.range = Some([30_000, 30_006].into());
        env.ports.map.insert("web:80".into(), 30_000);
    }))
}

#[test]
fn compose_reads_the_configuration_as_the_env_project() {
    // `${COMPOSE_PROJECT_NAME}` must name the project the stack runs as,
    // not the worktree's directory.
    let fx = Fixture::new();
    let mut env = env_with_named_ports(&fx, &json!({}));
    regenerate(&fx, &mut env, &[]);
    let projects: Vec<Option<String>> = fx
        .runner
        .compose_calls()
        .into_iter()
        .filter(|call| call.verb == "config")
        .map(|call| call.project)
        .collect();
    assert_eq!(projects, [Some(format!("{PROJECT}-feat-a"))]);
}

#[test]
fn a_named_port_reaches_compose_with_the_env_port() {
    let fx = Fixture::new();
    let mut env = env_with_named_ports(&fx, &json!({"web": "web:80"}));
    regenerate(&fx, &mut env, &[]);
    let calls = fx.runner.config_calls();
    assert_eq!(calls.len(), 1, "the ports did not change: one reading");
    assert_eq!(calls[0].env_var("RAMET_PORT_WEB").as_deref(), Some("30000"));
}

#[test]
fn a_named_port_is_unset_in_an_env_keeping_the_project_ports() {
    // As for a developer without ramet: `${RAMET_PORT_WEB:-8080}` gives the
    // project's port, and a value left in the shell cannot leak in.
    let fx = Fixture::new();
    write_settings(&fx.clone, &json!({"ports": {"web": "web:80"}}));
    fx.runner.set_config(example_config());
    let mut env = fx.save_env(fx.env("main", |env| {
        env.worktree.clone_from(&fx.clone);
        env.ports.map.insert("web:80".into(), 8080);
    }));
    regenerate(&fx, &mut env, &[]);
    let cmd = &fx.runner.config_calls()[0];
    assert_eq!(cmd.env_var("RAMET_PORT_WEB"), None);
    assert!(
        cmd.removed_env_vars()
            .iter()
            .any(|key| key == "RAMET_PORT_WEB"),
        "{:?}",
        cmd.removed_env_vars()
    );
}

#[test]
fn a_port_given_while_regenerating_is_read_again_with_its_value() {
    let fx = Fixture::new();
    let mut env = env_with_named_ports(&fx, &json!({"web": "web:80"}));
    env.ports.map.clear();
    regenerate(&fx, &mut env, &[]);
    let given = fx.load("feat-a").ports.map["web:80"];
    let calls = fx.runner.config_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].env_var("RAMET_PORT_WEB"), None);
    assert_eq!(calls[1].env_var("RAMET_PORT_WEB"), Some(given.to_string()));
}

#[test]
fn a_named_port_that_is_not_published_is_reported() {
    let fx = Fixture::new();
    let mut env = env_with_named_ports(&fx, &json!({"web": "proxy:80"}));
    regenerate(&fx, &mut env, &[]);
    let stderr = fx.stderr();
    assert!(
        stderr.contains("ports.web")
            && stderr.contains("proxy:80")
            && stderr.contains("RAMET_PORT_WEB"),
        "{stderr}"
    );
    assert!(fx.stdout().is_empty(), "compose output stays clean");
}

// ------------------------------------------------------------------ volumes

#[test]
fn a_volume_named_to_leave_its_directory_is_refused() {
    // Compose accepts `..` as a volume name. Stored as a directory of that
    // name, it would bind the env's own directory, env.json included, into a
    // container.
    let fx = Fixture::new();
    let mut env = env_with_settings(&fx, &json!({}));
    fx.runner.set_config(json!({
        "services": {"app": {"image": "alpine", "volumes": [
            {"type": "volume", "source": "..", "target": "/data"}
        ]}},
        "volumes": {"..": {"name": "demo_.."}},
    }));

    let err = env.regenerate(&fx.ctx(), &[]).unwrap_err();

    assert_matches!(err, Error::InvalidVolumeName { ref name } if name == "..");
    assert!(err.hint().unwrap().contains("rename it"));
    assert!(
        !fx.layout().compose_file(PROJECT, "main").exists(),
        "no configuration generated"
    );
}
