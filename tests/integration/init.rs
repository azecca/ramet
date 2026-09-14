//! `ramet init`: refusals, space check, migration source, stack to stop.

use ramet::commands::{Outcome, init};
use ramet::docker::Docker;
use ramet::error::{Error, Result};
use ramet::process::Output;
use serde_json::{Value, json};
use std::assert_matches;

use crate::support::{Fixture, full_message};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

fn run(fx: &Fixture, customize: impl FnOnce(&mut init::Args)) -> Result<Outcome> {
    let mut args = init::Args::default();
    customize(&mut args);
    init::run(&fx.ctx(), &args)
}

/// A repository whose compose project `name` declares `volumes`.
fn project(name: &str, volumes: &[&str]) -> Fixture {
    let fx = Fixture::new();
    let declared: serde_json::Map<String, Value> = volumes
        .iter()
        .map(|&volume| {
            (
                volume.to_owned(),
                json!({"name": format!("{name}_{volume}")}),
            )
        })
        .collect();
    fx.runner
        .set_config(json!({"name": name, "services": {}, "volumes": declared}));
    fx
}

/// Docker invocations that copy a volume, as `source:/from` mounts.
fn copies(fx: &Fixture) -> Vec<String> {
    fx.runner
        .external_argvs()
        .into_iter()
        .filter(|argv| argv.iter().any(|arg| arg == "cp"))
        .filter_map(|argv| argv.into_iter().find(|arg| arg.ends_with(":/from")))
        .collect()
}

fn stops(fx: &Fixture) -> Vec<Vec<String>> {
    fx.runner
        .external_argvs()
        .into_iter()
        .filter(|argv| argv.iter().any(|arg| arg == "down"))
        .collect()
}

// ---------------------------------------------------------------- refusals

#[test]
fn refuses_outside_the_main_clone() {
    let fx = Fixture::new();
    let worktree = fx.add_worktree("feat-a");
    let err = init::run(&fx.ctx_at(&worktree), &init::Args::default()).unwrap_err();
    assert_matches!(err, Error::InitOutsideMainClone { .. });
}

#[test]
fn refuses_a_project_already_initialized() {
    let fx = Fixture::with_main_env();
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::AlreadyInitialized { .. });
}

#[test]
fn refuses_outside_a_repository() {
    let fx = Fixture::new();
    let outside = fx.base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let err = init::run(&fx.ctx_at(&outside), &init::Args::default()).unwrap_err();
    assert_matches!(err, Error::NotInRepository { .. });
}

#[test]
fn from_a_subdirectory_names_the_compose_file_it_holds() {
    // The repository's root has no compose file; `example/` has one.
    let fx = Fixture::new();
    crate::support::git(&fx.clone, &["mv", "compose.yml", "example.yml"]);
    let example = fx.clone.join("example");
    std::fs::create_dir(&example).unwrap();
    std::fs::write(example.join("compose.yml"), "services: {}\n").unwrap();

    let err = init::run(&fx.ctx_at(&example), &init::Args::default()).unwrap_err();
    assert_matches!(err, Error::ComposeNotDiscoverable { .. });
    let message = full_message(&err);
    assert!(
        message.contains(&format!(
            "no compose file at the root of {}",
            fx.clone.display()
        )),
        "{message}"
    );
    assert!(message.contains("whole git repositories"), "{message}");
    assert!(
        message
            .contains(r#".ramet.json, at its root: {"compose":{"files":["example/compose.yml"]}}"#),
        "{message}"
    );
}

#[test]
fn from_the_root_keeps_the_general_advice() {
    let fx = Fixture::new();
    crate::support::git(&fx.clone, &["mv", "compose.yml", "example.yml"]);
    let err = run(&fx, |_| {}).unwrap_err();
    let hint = err.hint().unwrap();
    assert!(
        hint.contains(
            r#"{"compose":{"files":["docker/compose/base.yml","docker/compose/dev.yml"]}}"#
        ),
        "{hint}"
    );
}

#[test]
fn an_invalid_ramet_json_is_refused_with_its_shape() {
    let fx = Fixture::new();
    std::fs::write(fx.clone.join(".ramet.json"), r#"{"compose": {"file": []}}"#).unwrap();
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::InvalidSettings { .. });
    let message = full_message(&err);
    assert!(message.contains("unknown field `file`"), "{message}");
    assert!(message.contains(r#""profiles": ["dev"]"#), "{message}");
}

#[test]
fn refuses_when_the_project_directory_exists() {
    let fx = project("demo", &[]);
    std::fs::create_dir(fx.root.join("demo")).unwrap();
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::AlreadyInitialized { .. });
}

// --------------------------------------------------------- main env name
// The env of the main clone is named after the repository's default branch,
// not after whatever branch is checked out when `init` runs.

fn git(fx: &Fixture, args: &[&str]) {
    crate::support::git(&fx.clone, args);
}

#[test]
fn the_main_env_takes_the_name_of_the_default_branch() {
    let fx = project("demo", &[]);
    git(&fx, &["branch", "-m", "main", "master"]);
    run(&fx, |_| {}).unwrap();
    assert_eq!(fx.env_names(), ["master"]);
}

#[test]
fn the_default_branch_of_origin_comes_first() {
    let fx = project("demo", &[]);
    git(&fx, &["update-ref", "refs/remotes/origin/develop", "HEAD"]);
    git(
        &fx,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ],
    );
    git(&fx, &["checkout", "-q", "-b", "develop"]);
    run(&fx, |_| {}).unwrap();
    assert_eq!(fx.env_names(), ["develop"]);
}

#[test]
fn on_another_branch_the_name_is_confirmed_in_a_terminal() {
    let fx = project("demo", &[]);
    git(&fx, &["checkout", "-q", "-b", "feat-login"]);
    fx.answer("\n");
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    let out = fx.stdout();
    assert!(
        out.contains("the main clone is on \"feat-login\", not on the default branch \"main\""),
        "{out}"
    );
    assert!(out.contains("name the main env \"main\"? [Y/n]"), "{out}");
    assert_eq!(fx.env_names(), ["main"]);
}

#[test]
fn declining_the_name_changes_nothing() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata"]);
    git(&fx, &["checkout", "-q", "-b", "feat-login"]);
    fx.answer("n\n");
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Aborted);
    assert!(fx.env_names().is_empty());
    assert!(copies(&fx).is_empty() && stops(&fx).is_empty());
    assert!(fx.stdout().contains("--name"), "{}", fx.stdout());
}

#[test]
fn on_another_branch_without_a_terminal_the_name_must_be_given() {
    let fx = project("demo", &[]);
    git(&fx, &["checkout", "-q", "-b", "feat-login"]);
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::MainEnvNameRequired { .. });
    let message = full_message(&err);
    assert!(message.contains("on \"feat-login\""), "{message}");
    assert!(message.contains("ramet init --name main"), "{message}");
    assert!(fx.env_names().is_empty());
}

#[test]
fn a_detached_head_counts_as_another_branch() {
    let fx = project("demo", &[]);
    git(&fx, &["checkout", "-q", "--detach"]);
    let err = run(&fx, |_| {}).unwrap_err();
    assert!(err.to_string().contains("on a detached HEAD"), "{err}");
}

#[test]
fn a_given_name_needs_no_confirmation() {
    let fx = project("demo", &[]);
    git(&fx, &["checkout", "-q", "-b", "feat-login"]);
    run(&fx, |args| args.name = Some("trunk".into())).unwrap();
    assert_eq!(fx.env_names(), ["trunk"]);
}

#[test]
fn without_a_recognizable_default_branch_the_env_is_named_main() {
    let fx = project("demo", &[]);
    git(&fx, &["branch", "-m", "main", "trunk"]);
    git(&fx, &["branch", "other"]);
    run(&fx, |_| {}).unwrap();
    assert_eq!(fx.env_names(), ["main"]);
    assert!(
        fx.stdout().contains("no default branch found"),
        "{}",
        fx.stdout()
    );
}

#[test]
fn a_repository_without_commits_is_on_the_branch_head_names() {
    let fx = Fixture::new();
    let fresh = fx.base.join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    crate::support::git(&fresh, &["init", "-q", "-b", "trunk"]);
    assert_eq!(
        fx.ctx().git().default_branch(&fresh).as_deref(),
        Some("trunk")
    );
}

// ------------------------------------------------------------------ result

#[test]
fn creates_the_main_env_and_starts_it() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata"]);
    fx.runner.set_config(json!({
        "name": "demo",
        "services": {"web": {"ports": [crate::support::port(80, 8080)]}},
        "volumes": {"pgdata": {"name": "demo_pgdata"}},
    }));
    crate::support::write_settings(&fx.clone, &json!({"compose": {"profiles": ["debug"]}}));
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    let env = fx.load("main");
    assert!(env.is_primary());
    assert_eq!(env.worktree, fx.clone);
    assert_eq!(
        env.ports.range, None,
        "the main env keeps the project's ports"
    );
    assert_eq!(env.ports.map["web:80"], 8080);
    assert!(
        fx.runner.config_calls().iter().all(|cmd| cmd
            .argv()
            .windows(2)
            .any(|pair| pair == ["--profile", "debug"])),
        "the profiles of .ramet.json apply from the start"
    );
    assert_eq!(copies(&fx), ["demo_pgdata:/from"]);
    assert!(fx.runner.verbs_for("main").contains(&"up".to_owned()));
}

#[test]
fn a_stack_that_fails_to_start_is_stopped_before_its_data_goes() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata"]);
    fx.runner.fail("up");
    assert!(run(&fx, |_| {}).is_err());
    let verbs = fx.runner.verbs_for("main");
    let up = verbs
        .iter()
        .position(|verb| verb == "up")
        .expect("up tried");
    assert!(
        verbs[up..].iter().any(|verb| verb == "down"),
        "containers left on a deleted subvolume: {verbs:?}"
    );
    assert!(!fx.env_dir("main").exists());
}

#[test]
fn remap_ports_allocates_a_block_to_the_main_env() {
    let fx = project("demo", &[]);
    fx.runner.set_config(
        json!({"name": "demo", "services": {"web": {"ports": [crate::support::port(80, 8080)]}}}),
    );
    run(&fx, |args| args.remap_ports = true).unwrap();
    let env = fx.load("main");
    let range = env.ports.range.expect("a block");
    assert_eq!(env.ports.map["web:80"], range.first);
}

#[test]
fn name_chooses_the_main_env_name() {
    let fx = project("demo", &[]);
    run(&fx, |args| args.name = Some("Trunk".into())).unwrap();
    assert_eq!(fx.env_names(), ["trunk"]);
}

#[test]
fn empty_options_count_as_absent() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    fx.answer("y\n");
    run(&fx, |args| {
        args.name = Some(String::new());
        args.migrate_from = Some(String::new());
    })
    .unwrap();
    assert!(fx.layout().env_dir("usembic", "main").is_dir());
    // An empty prefix is no prefix: the migration source is still proposed.
    assert_eq!(copies(&fx), ["compose_postgres_data:/from"]);
}

// ------------------------------------------------------------------- space
// Measured before anything is touched: otherwise the copy fails with an
// unexplained "No space left on device", after the stack was stopped.

#[test]
fn refuses_volumes_that_do_not_fit() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata"]);
    fx.runner.set_volume_bytes(100 * GIB);
    fx.host.0.free_bytes.set(10 * GIB);
    let err = run(&fx, |_| {}).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("100.0 GiB") && message.contains("10.0 GiB"),
        "{message}"
    );
    assert!(
        stops(&fx).is_empty(),
        "the project must not be left stopped"
    );
}

#[test]
fn accepts_volumes_that_fit() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata"]);
    fx.runner.set_volume_bytes(100 * GIB);
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    assert_eq!(copies(&fx), ["demo_pgdata:/from"]);
}

// ------------------------------------------------------------ stack to stop
// A bare `docker compose down` would resolve the files of the current
// directory and could stop an unrelated project.

#[test]
fn stops_the_resolved_project_by_name() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata", "compose_pgdata"]);
    run(&fx, |_| {}).unwrap();
    assert_eq!(
        stops(&fx),
        [[
            "docker",
            "compose",
            "-p",
            "demo",
            "down",
            "--remove-orphans"
        ]]
    );
}

#[test]
fn stops_the_project_whose_volumes_are_migrated() {
    let fx = project("demo", &["pgdata"]);
    fx.runner.set_volumes(&["demo_pgdata", "compose_pgdata"]);
    run(&fx, |args| args.migrate_from = Some("compose".into())).unwrap();
    assert_eq!(stops(&fx)[0][3], "compose");
}

// ---------------------------------------------------------- container_name

#[test]
fn reports_the_container_names_it_removes() {
    let fx = Fixture::new();
    fx.runner.set_config(json!({"name": "demo", "services": {
        "db": {"container_name": "app_db"}, "web": {"container_name": "app_web"}}}));
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    let out = fx.stdout();
    assert!(
        out.contains("`container_name` removed from the generated configuration for db, web"),
        "{out}"
    );
}

// ------------------------------------------------------- migration source
// Volumes carry the compose project name as a prefix, and the prefix depends
// on how compose was started. Never start on empty volumes silently; never
// choose among candidates automatically either.

/// The `usembic` project, whose data sleeps under other prefixes.
fn orphaned(volumes: &[&str], present: &[&str]) -> Fixture {
    let fx = project("usembic", volumes);
    fx.runner.set_volumes(present);
    fx.runner.set_volume_bytes(100 * MIB);
    fx
}

#[test]
fn files_in_a_subdirectory_find_the_volumes_of_their_own_command() {
    // `docker compose -f docker/compose/base.yml up` names the project and
    // its volumes `compose`, and so does ramet, resolving the files the same
    // way: the data is found without a question. The ramet project keeps
    // the repository's name, which says more than `compose`.
    let fx = Fixture::new();
    crate::support::write_file(&fx.clone, "docker/compose/base.yml", "services: {}\n");
    crate::support::write_settings(
        &fx.clone,
        &json!({"compose": {"files": ["docker/compose/base.yml"]}}),
    );
    fx.runner
        .set_config(json!({"name": "compose", "services": {},
        "volumes": {"postgres_data": {"name": "compose_postgres_data"}}}));
    fx.runner.set_volumes(&["compose_postgres_data"]);
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    assert_eq!(copies(&fx), ["compose_postgres_data:/from"]);
    assert_eq!(
        stops(&fx)[0][3],
        "compose",
        "the stack of the user's command"
    );
    assert!(
        fx.layout().env_dir("app", "main").is_dir(),
        "{}",
        fx.stdout()
    );
}

#[test]
fn a_name_the_project_gives_itself_is_kept() {
    let fx = Fixture::new();
    crate::support::write_file(&fx.clone, "docker/compose/base.yml", "services: {}\n");
    crate::support::write_settings(
        &fx.clone,
        &json!({"compose": {"files": ["docker/compose/base.yml"]}}),
    );
    fx.runner
        .set_config(json!({"name": "shop", "services": {}}));
    run(&fx, |_| {}).unwrap();
    assert!(fx.layout().env_dir("shop", "main").is_dir());
}

#[test]
fn the_view_shows_what_would_be_taken() {
    let fx = orphaned(
        &["postgres_data", "minio_data"],
        &[
            "compose_postgres_data",
            "compose_minio_data",
            "vortex_postgres_data",
        ],
    );
    fx.answer("n\n");
    run(&fx, |_| {}).unwrap();
    let out = fx.stdout();
    for expected in [
        "minio_data",
        "compose_minio_data",
        "compose_postgres_data",
        "total",
    ] {
        assert!(out.contains(expected), "{expected} missing from:\n{out}");
    }
    // The rest goes to a note, not on the same footing.
    let vortex: Vec<&str> = out.lines().filter(|line| line.contains("vortex")).collect();
    assert_eq!(vortex.len(), 1, "{out}");
    assert!(vortex[0].contains("not be touched"), "{out}");
}

#[test]
fn asks_when_exactly_one_prefix_holds_every_volume() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    fx.answer("y\n");
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
    assert!(fx.stdout().contains("migrate from \"compose\"?"));
    assert_eq!(copies(&fx), ["compose_postgres_data:/from"]);
    assert_eq!(stops(&fx)[0][3], "compose");
}

#[test]
fn declining_the_proposal_aborts() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    fx.answer("n\n");
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Aborted);
    assert!(copies(&fx).is_empty());
    assert!(stops(&fx).is_empty());
}

#[test]
fn without_a_terminal_the_options_are_given() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::MigrationChoiceRequired { .. });
    assert!(full_message(&err).contains("--migrate-from compose"));
    assert!(full_message(&err).contains("--no-migrate"));
}

#[test]
fn only_prefixes_holding_every_volume_are_proposed() {
    let fx = orphaned(
        &["postgres_data", "minio_data"],
        &[
            "compose_postgres_data",
            "compose_minio_data",
            "vortex_postgres_data",
        ],
    );
    let message = full_message(&run(&fx, |_| {}).unwrap_err());
    assert!(message.contains("--migrate-from compose"), "{message}");
    assert!(!message.contains("vortex"), "{message}");
}

#[test]
fn a_prefix_lacking_a_volume_that_exists_is_never_proposed() {
    // `usembic_pgdata` holds the data and `usembic_uploads` was never created;
    // `legacy_uploads` belongs to an unrelated project. Proposing `legacy`
    // would copy `pgdata` from nothing and stop that other project.
    let fx = orphaned(
        &["pgdata", "uploads"],
        &["usembic_pgdata", "legacy_uploads"],
    );
    fx.answer("y\n");
    let err = run(&fx, |_| {}).unwrap_err();
    assert_matches!(err, Error::MigrationChoiceRequired { .. });
    let message = full_message(&err);
    assert!(
        message.contains("No prefix holds every volume"),
        "{message}"
    );
    assert!(!message.contains("--migrate-from legacy"), "{message}");
    assert!(stops(&fx).is_empty());
    let measured = fx
        .runner
        .external_argvs()
        .iter()
        .any(|argv| argv.iter().any(|arg| arg == "legacy_pgdata:/v"));
    assert!(!measured, "measuring would have created `legacy_pgdata`");
}

#[test]
fn no_migrate_still_copies_the_volumes_that_exist() {
    let fx = orphaned(
        &["pgdata", "uploads"],
        &["usembic_pgdata", "legacy_uploads"],
    );
    assert_eq!(
        run(&fx, |args| args.no_migrate = true).unwrap(),
        Outcome::Done
    );
    assert_eq!(copies(&fx), ["usembic_pgdata:/from"]);
    assert_eq!(stops(&fx)[0][3], "usembic");
}

#[test]
fn an_ambiguous_choice_lists_every_complete_prefix() {
    let fx = orphaned(
        &["postgres_data"],
        &["compose_postgres_data", "vortex_postgres_data"],
    );
    let err = run(&fx, |_| {}).unwrap_err();
    let message = full_message(&err);
    for expected in [
        "compose_postgres_data",
        "vortex_postgres_data",
        "--migrate-from",
        "--no-migrate",
    ] {
        assert!(
            message.contains(expected),
            "{expected} missing from:\n{message}"
        );
    }
    assert!(
        stops(&fx).is_empty(),
        "a refusal leaves the project running"
    );
}

#[test]
fn no_migrate_starts_on_empty_volumes() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    assert_eq!(
        run(&fx, |args| args.no_migrate = true).unwrap(),
        Outcome::Done
    );
    assert!(copies(&fx).is_empty());
    assert!(fx.stdout().contains("nothing to migrate (never created)"));
}

#[test]
fn migrate_from_copies_from_that_prefix() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    assert_eq!(
        run(&fx, |args| args.migrate_from = Some("compose".into())).unwrap(),
        Outcome::Done
    );
    assert_eq!(copies(&fx), ["compose_postgres_data:/from"]);
}

#[test]
fn an_unknown_migrate_from_is_refused() {
    let fx = orphaned(&["postgres_data"], &["compose_postgres_data"]);
    let err = run(&fx, |args| args.migrate_from = Some("ghost".into())).unwrap_err();
    assert_matches!(err, Error::MigrationSourceMissing { .. });
    assert!(err.to_string().contains("ghost_postgres_data"));
}

#[test]
fn without_any_candidate_it_starts_quietly() {
    let fx = orphaned(&["postgres_data"], &[]);
    assert_eq!(run(&fx, |_| {}).unwrap(), Outcome::Done);
}

// ----------------------------------------------------------------- helpers

#[test]
fn volume_size_reads_kibibytes() {
    let fx = Fixture::new();
    fx.runner.set_volumes(&["demo_pgdata"]);
    fx.runner.handle(|cmd| {
        cmd.argv()
            .contains(&"du".to_owned())
            .then(|| Output::success_with("123456\t/v\n"))
    });
    let ctx = fx.ctx();
    assert_eq!(
        Docker::new(ctx.runner()).volume_size("demo_pgdata"),
        123_456 * 1024
    );
}

#[test]
fn volume_size_is_zero_when_it_cannot_be_measured() {
    let fx = Fixture::new();
    fx.runner.handle(|_| Some(Output::failure(1, "")));
    let ctx = fx.ctx();
    assert_eq!(Docker::new(ctx.runner()).volume_size("absent"), 0);
}

#[test]
fn volume_size_never_creates_a_missing_volume() {
    // `docker run -v absent:/v` would create `absent`.
    let fx = Fixture::new();
    let ctx = fx.ctx();
    assert_eq!(Docker::new(ctx.runner()).volume_size("absent"), 0);
    let started = fx
        .runner
        .external_argvs()
        .iter()
        .any(|argv| argv.get(1).is_some_and(|verb| verb == "run"));
    assert!(!started);
}
