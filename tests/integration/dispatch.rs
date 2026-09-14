//! Running a parsed command line: `ramet compose`, mistaken command lines,
//! the sudo guard and the data volume mount.

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;

use crate::support::{Fixture, PROJECT};

/// Runs `ramet <args>` from `dir` and returns its exit code.
fn ramet_in(fx: &Fixture, dir: &std::path::Path, args: &[&str]) -> u8 {
    let cli = Cli::try_parse_from(std::iter::once("ramet").chain(args.iter().copied())).unwrap();
    app::run(&fx.ctx_at(dir), &cli.command)
}

fn ramet(fx: &Fixture, args: &[&str]) -> u8 {
    ramet_in(fx, &fx.clone, args)
}

/// Makes the data root look mounted and writable.
fn mounted(fx: &Fixture) {
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        (argv[0] == "findmnt" && argv.contains(&"--target".to_owned()))
            .then(|| ramet::process::Output::success_with("btrfs\n"))
    });
}

fn mount_attempts(fx: &Fixture) -> usize {
    fx.runner
        .external_argvs()
        .iter()
        .filter(|argv| argv[0] == "mount")
        .count()
}

// ---------------------------------------------------------- ramet compose

#[test]
fn a_compose_command_goes_to_the_env_stack_with_its_arguments() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    assert_eq!(
        ramet(&fx, &["compose", "logs", "-f", "--tail", "50", "web"]),
        0
    );
    let call = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "logs")
        .unwrap();
    assert_eq!(call.project.as_deref(), Some("demo-main"));
    assert_eq!(call.args, ["-f", "--tail", "50", "web"]);
    let cmd = fx
        .runner
        .calls()
        .into_iter()
        .find(|cmd| cmd.argv().contains(&"logs".to_owned()))
        .unwrap();
    assert_eq!(
        cmd.mode(),
        ramet::process::OutputMode::Inherited,
        "the user sees compose live"
    );
}

#[test]
fn the_exit_code_of_compose_is_forwarded() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    fx.runner.fail("exec");
    assert_eq!(ramet(&fx, &["compose", "exec", "db", "false"]), 1);
}

#[test]
fn a_one_off_profile_reaches_the_configuration_but_not_the_env() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    ramet(&fx, &["compose", "run", "--profile", "tools", "migrate"]);
    let config = &fx.runner.config_calls()[0].argv();
    assert!(
        config.windows(2).any(|pair| pair == ["--profile", "tools"]),
        "{config:?}"
    );
    assert!(
        !fx.clone.join(ramet::settings::FILE_NAME).exists(),
        "nothing is recorded"
    );
}

#[test]
fn the_target_is_named_on_a_terminal() {
    // `ramet compose` is the only command that never names the env it acts on.
    let fx = Fixture::with_main_env();
    mounted(&fx);
    fx.stderr_is_terminal();
    ramet(&fx, &["compose", "ps"]);
    assert!(
        fx.stderr().contains(&format!("· {PROJECT}/main (main)")),
        "{}",
        fx.stderr()
    );
}

#[test]
fn the_target_is_not_named_for_scripts() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    ramet(&fx, &["compose", "ps"]);
    assert_eq!(fx.stderr(), "");
}

#[test]
fn the_context_line_names_a_detached_head() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    fx.stderr_is_terminal();
    let raw = fx.add_worktree("raw");
    fx.save_env(fx.env("raw", |env| {
        env.parent = Some("main".into());
        env.worktree = raw.clone();
    }));
    crate::support::git(&raw, &["checkout", "-q", "--detach"]);
    ramet_in(&fx, &raw, &["compose", "ps"]);
    assert!(fx.stderr().contains("(detached HEAD)"), "{}", fx.stderr());
}

// ------------------------------------------------- mistaken command lines
// Explained, never run: nothing may reach compose, nor mount the volume.

/// Runs `ramet <args>`, expects it to fail without running anything, and
/// returns what it printed on standard error.
fn refused(fx: &Fixture, args: &[&str]) -> String {
    assert_eq!(ramet(fx, args), 1);
    assert!(
        fx.runner.external_argvs().is_empty(),
        "nothing may run: {:?}",
        fx.runner.external_argvs()
    );
    fx.stderr()
}

#[test]
fn a_typo_gets_a_suggestion() {
    let fx = Fixture::with_main_env();
    let err = refused(&fx, &["checkpint", "c1"]);
    assert!(err.contains("unknown command \"checkpint\""), "{err}");
    assert!(err.contains("did you mean `ramet checkpoint`?"), "{err}");
}

#[test]
fn a_compose_command_without_compose_gets_the_line_to_type() {
    let fx = Fixture::with_main_env();
    let err = refused(&fx, &["logs", "-f", "web"]);
    assert!(
        err.contains("\"logs\" is a docker compose command"),
        "{err}"
    );
    assert!(err.contains("`ramet compose logs -f web`"), "{err}");
}

#[test]
fn an_unrelated_word_points_to_the_help() {
    let fx = Fixture::with_main_env();
    let err = refused(&fx, &["zzzqqq"]);
    assert!(err.contains("unknown command \"zzzqqq\""), "{err}");
    assert!(err.contains("`ramet --help`"), "{err}");
}

#[test]
fn a_ramet_command_given_to_compose_is_refused() {
    let fx = Fixture::with_main_env();
    let err = refused(&fx, &["compose", "checkpoint", "c1"]);
    assert!(err.contains("\"checkpoint\" is a ramet command"), "{err}");
    assert!(err.contains("`ramet checkpoint c1`"), "{err}");
}

#[test]
fn a_mistyped_compose_command_gets_a_suggestion() {
    let fx = Fixture::with_main_env();
    let err = refused(&fx, &["compose", "dwn"]);
    assert!(err.contains("did you mean `ramet compose down`?"), "{err}");
}

#[test]
fn an_unknown_compose_command_is_left_to_compose() {
    // A newer compose may know commands ramet does not.
    let fx = Fixture::with_main_env();
    mounted(&fx);
    ramet(&fx, &["compose", "zzzqqq"]);
    assert!(
        fx.runner
            .compose_calls()
            .iter()
            .any(|call| call.verb == "zzzqqq")
    );
}

#[test]
fn compose_reaches_the_commands_ramet_shares_a_name_with() {
    let fx = Fixture::with_main_env();
    mounted(&fx);
    assert_eq!(ramet(&fx, &["compose", "rm", "-f"]), 0);
    let call = fx
        .runner
        .compose_calls()
        .into_iter()
        .find(|call| call.verb == "rm")
        .expect("`docker compose rm` reached");
    assert_eq!(call.project.as_deref(), Some("demo-main"));
    assert_eq!(fx.env_names(), ["main"], "no env was removed");
}

// ------------------------------------------------------------------ mount

#[test]
fn compose_mounts_the_data_volume() {
    let fx = Fixture::with_main_env();
    ramet(&fx, &["compose", "up"]);
    assert_eq!(mount_attempts(&fx), 1);
}

#[test]
fn prompt_never_mounts_the_data_volume() {
    let fx = Fixture::with_main_env();
    assert_eq!(ramet(&fx, &["prompt"]), 0);
    assert_eq!(fx.stdout(), "");
    assert_eq!(mount_attempts(&fx), 0);
}

#[test]
fn doctor_never_mounts_the_data_volume() {
    let fx = Fixture::with_main_env();
    ramet(&fx, &["doctor"]);
    assert_eq!(mount_attempts(&fx), 0);
}

#[test]
fn print_fstab_prints_the_line_only() {
    let fx = Fixture::new();
    assert_eq!(ramet(&fx, &["doctor", "--print-fstab"]), 0);
    assert_eq!(fx.stdout(), fx.layout().fstab_line());
}

// -------------------------------------------------------------------- sudo
// Nothing needs privileges any more: `sudo ramet new feat-x` would leave
// root-owned subvolumes the user can no longer write to.

#[test]
fn refuses_sudo_from_a_regular_account() {
    let fx = Fixture::with_main_env();
    fx.host.0.euid.set(0);
    fx.host
        .0
        .vars
        .borrow_mut()
        .insert("SUDO_USER".into(), "alex".into());
    assert_eq!(ramet(&fx, &["compose", "up", "-d"]), 1);
    let err = fx.stderr();
    assert!(
        err.contains("sudo") && err.contains("root") && err.contains("alex"),
        "{err}"
    );
    assert!(
        fx.runner.external_argvs().is_empty(),
        "nothing runs after the refusal"
    );
}

#[test]
fn a_genuinely_root_environment_is_let_through() {
    let fx = Fixture::with_main_env();
    fx.host.0.euid.set(0);
    mounted(&fx);
    assert_eq!(ramet(&fx, &["compose", "ps"]), 0);
}

#[test]
fn doas_and_pkexec_are_refused_as_sudo_is() {
    for (variable, value) in [("DOAS_USER", "alex"), ("PKEXEC_UID", "1000")] {
        let fx = Fixture::with_main_env();
        fx.host.0.euid.set(0);
        mounted(&fx);
        fx.host
            .0
            .vars
            .borrow_mut()
            .insert(variable.into(), value.into());
        assert_eq!(ramet(&fx, &["compose", "ps"]), 1, "{variable}");
        assert!(fx.stderr().contains("root"), "{}", fx.stderr());
    }
}

#[test]
fn doctor_under_sudo_leaves_the_project_alone() {
    // Checking it runs git and compose on the user's files, as root.
    let fx = Fixture::with_main_env();
    mounted(&fx);
    fx.host.0.euid.set(0);
    fx.host
        .0
        .vars
        .borrow_mut()
        .insert("SUDO_USER".into(), "alex".into());
    ramet(&fx, &["doctor"]);
    assert!(
        fx.stdout().contains("not checked under sudo"),
        "{}",
        fx.stdout()
    );
    assert!(
        fx.runner
            .argvs()
            .iter()
            .all(|argv| argv[0] != "git" || argv[1] == "--version"),
        "{:?}",
        fx.runner.argvs()
    );
}

#[test]
fn doctor_stays_allowed_under_sudo() {
    let fx = Fixture::new();
    fx.host.0.euid.set(0);
    fx.host
        .0
        .vars
        .borrow_mut()
        .insert("SUDO_USER".into(), "alex".into());
    assert_eq!(ramet(&fx, &["doctor", "--print-fstab"]), 0);
}

// ------------------------------------------------------------------ errors

#[test]
fn errors_go_to_standard_error_with_their_hint() {
    let fx = Fixture::new();
    mounted(&fx);
    assert_eq!(ramet(&fx, &["ls"]), 1);
    assert_eq!(fx.stdout(), "");
    let err = fx.stderr();
    assert!(
        err.starts_with("error: no ramet env for this repository"),
        "{err}"
    );
    assert!(err.contains("hint: run `ramet init`"), "{err}");
}
