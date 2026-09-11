//! Command line parsing.
//!
//! ramet's own commands are subcommands. Docker compose commands go through
//! one of them, `ramet compose`, with their arguments untouched: `ramet
//! compose up -d`, `ramet compose logs -f web`. Keeping them apart means no
//! ramet command ever hides a compose one (`ls`, `rm` exist in both), and a
//! word that is neither is explained rather than run.

use std::borrow::Cow;
use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand};

use crate::commands::{
    checkpoint, deinit, df, doctor, init, log, ls, new, passthrough, path, prompt, prune, restore,
    rm, setup, sync,
};
use crate::error::{Error, Result};
use crate::process::shell_quote;
use crate::util::similarity::closest;

/// Docker compose subcommands known when this was written. Only used to
/// explain a mistaken command line: a word missing from this list still
/// reaches compose through `ramet compose` when it resembles nothing known,
/// so that verbs added by a newer compose are not blocked.
pub const COMPOSE_COMMANDS: [&str; 35] = [
    "attach", "build", "commit", "config", "cp", "create", "down", "events", "exec", "export",
    "help", "images", "kill", "logs", "ls", "pause", "port", "ps", "publish", "pull", "push",
    "restart", "rm", "run", "scale", "start", "stats", "stop", "top", "unpause", "up", "version",
    "volumes", "wait", "watch",
];

/// One git branch, one docker stack, data of its own.
#[derive(Debug, Parser)]
#[command(
    name = "ramet",
    arg_required_else_help = true,
    subcommand_required = true,
    // `--keep` for `--keep-checkpoints`: an unambiguous prefix is enough.
    infer_long_args = true,
    after_help = "Docker compose commands run on the current env through `ramet compose`: \
                  ramet compose up -d, ramet compose logs -f web…"
)]
pub struct Cli {
    /// Log every external command
    #[arg(short, long)]
    pub verbose: bool,

    /// What to do
    #[command(subcommand)]
    pub command: Command,
}

/// A ramet command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Prepare the data volume, once per machine
    Setup(setup::Args),
    /// Check the prerequisites and report inconsistencies
    Doctor(doctor::Args),
    /// Initialize the project and migrate its docker volumes
    Init(init::Args),
    /// Clone the current env into a new env
    New(new::Args),
    /// List the envs of the project
    Ls(ls::Args),
    /// Take a return point of the current env's data
    Checkpoint(checkpoint::Args),
    /// List the checkpoints of the current env
    Log(log::Args),
    /// Rewind the current env's data to a checkpoint
    Restore(restore::Args),
    /// Remove an env, its worktree and its checkpoints
    Rm(rm::Args),
    /// Copy the local files (.env…) of another env again, ports rewritten
    Sync(sync::Args),
    /// Undo `init`: hand the project back
    Deinit(deinit::Args),
    /// Show how full the data volume is, and what fills it
    Df(df::Args),
    /// Delete the envs and checkpoints whose worktree is gone
    Prune(prune::Args),
    /// Print the worktree of an env, for `cd`
    Path(path::Args),
    /// Print `project:env` for the shell prompt
    Prompt(prompt::Args),
    /// Run a docker compose command on the current env's stack
    Compose(passthrough::Args),
    /// Any other first word, with what follows: never run, only explained.
    #[command(external_subcommand)]
    Unknown(Vec<OsString>),
}

impl Command {
    /// The word the user typed to select the command.
    pub fn name(&self) -> Cow<'_, str> {
        let name = match self {
            Self::Setup(_) => "setup",
            Self::Doctor(_) => "doctor",
            Self::Init(_) => "init",
            Self::New(_) => "new",
            Self::Ls(_) => "ls",
            Self::Checkpoint(_) => "checkpoint",
            Self::Log(_) => "log",
            Self::Restore(_) => "restore",
            Self::Rm(_) => "rm",
            Self::Sync(_) => "sync",
            Self::Deinit(_) => "deinit",
            Self::Df(_) => "df",
            Self::Prune(_) => "prune",
            Self::Path(_) => "path",
            Self::Prompt(_) => "prompt",
            Self::Compose(_) => "compose",
            Self::Unknown(args) => {
                return args
                    .first()
                    .map_or(Cow::Borrowed(""), |word| word.to_string_lossy());
            }
        };
        Cow::Borrowed(name)
    }

    /// Whether the command needs the data volume mounted.
    ///
    /// `setup` prepares the volume and mounts it itself, `doctor` diagnoses
    /// without changing anything, and `prompt` runs at every shell prompt:
    /// none of them triggers the mount beforehand.
    pub fn needs_data_volume(&self) -> bool {
        !matches!(
            self,
            Self::Setup(_) | Self::Doctor(_) | Self::Prompt(_) | Self::Unknown(_)
        )
    }

    /// Refuses, before anything runs, a command line that cannot be meant:
    /// a word that is no ramet command, a mistyped compose command, or a
    /// ramet command given to compose.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Unknown(args) => Err(unknown_command(args)),
            Self::Compose(args) => check_compose_command(&args.args),
            _ => Ok(()),
        }
    }
}

/// Names of ramet's own commands.
pub fn ramet_commands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect()
}

/// Why `args`, whose first word is none of ramet's commands, cannot run.
///
/// A docker compose command typed out of habit gets the exact command line to
/// type instead; a typo gets the command it resembles, ramet's or compose's.
pub fn unknown_command(args: &[OsString]) -> Error {
    let word = first_word(args);
    if COMPOSE_COMMANDS.contains(&word.as_str()) {
        return Error::BareComposeCommand {
            word,
            command: command_line("ramet compose", args),
        };
    }
    let ramet = ramet_commands();
    let known = ramet.iter().map(String::as_str).chain(COMPOSE_COMMANDS);
    let suggestion = closest(&word, known).map(|known| {
        if ramet.iter().any(|command| command == known) {
            format!("ramet {known}")
        } else {
            format!("ramet compose {known}")
        }
    });
    Error::UnknownCommand { word, suggestion }
}

/// Refuses a docker compose command line that cannot be meant.
///
/// `ramet compose checkpoint` is ramet's own command typed in the wrong
/// place, and forwarding `ramet compose dwn` would only print compose's whole
/// help without saying what is wrong. A word that resembles nothing known
/// goes to compose, which may know it; so does a line that starts with an
/// option, whose command is harder to spot.
pub fn check_compose_command(args: &[OsString]) -> Result<()> {
    let word = first_word(args);
    if word.starts_with('-') || COMPOSE_COMMANDS.contains(&word.as_str()) {
        return Ok(());
    }
    let verb = closest(&word, COMPOSE_COMMANDS);
    if ramet_commands().contains(&word) {
        return Err(Error::RametCommandInCompose {
            word,
            command: command_line("ramet", args),
            // `ramet compose log` more likely means compose's `logs`.
            compose: verb.map(|verb| format!("ramet compose {verb}")),
        });
    }
    match verb {
        Some(verb) => Err(Error::UnknownCommand {
            word,
            suggestion: Some(format!("ramet compose {verb}")),
        }),
        None => Ok(()),
    }
}

fn first_word(args: &[OsString]) -> String {
    args.first()
        .map(|word| word.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `prefix` followed by `args`, quoted for a shell where needed.
fn command_line(prefix: &str, args: &[OsString]) -> String {
    args.iter()
        .map(|arg| shell_quote(&arg.to_string_lossy()))
        .fold(prefix.to_owned(), |line, arg| line + " " + &arg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("ramet").chain(args.iter().copied())).unwrap()
    }

    fn forwarded(cli: &Cli) -> Vec<String> {
        match &cli.command {
            Command::Compose(args) => lossy(&args.args),
            other => panic!("not forwarded: {other:?}"),
        }
    }

    fn lossy(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_compose_command_is_forwarded_with_its_arguments() {
        assert_eq!(forwarded(&parse(&["compose", "up", "-d"])), ["up", "-d"]);
        assert_eq!(
            forwarded(&parse(&["compose", "logs", "-f", "--tail", "50", "web"])),
            ["logs", "-f", "--tail", "50", "web"]
        );
        assert_eq!(
            forwarded(&parse(&["compose", "--profile", "tools", "run", "migrate"])),
            ["--profile", "tools", "run", "migrate"]
        );
    }

    #[test]
    fn compose_needs_a_command() {
        assert!(Cli::try_parse_from(["ramet", "compose"]).is_err());
    }

    #[test]
    fn the_help_shows_compose_among_ramet_s_commands() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            help.contains("compose     Run a docker compose command"),
            "{help}"
        );
        assert!(help.contains("ramet compose up -d"), "{help}");
        // Before any compose command, `--help` is ramet's help for `compose`.
        let err = Cli::try_parse_from(["ramet", "compose", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(err.to_string().contains("ramet compose help"), "{err}");
    }

    #[test]
    fn help_after_compose_is_compose_s_own() {
        assert_eq!(
            forwarded(&parse(&["compose", "help", "up"])),
            ["help", "up"]
        );
        assert_eq!(
            forwarded(&parse(&["compose", "up", "--help"])),
            ["up", "--help"]
        );
        assert!(check_compose_command(&os(&["help", "up"])).is_ok());
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        let cli = parse(&["--verb", "rm", "feat-a", "--keep"]);
        assert!(cli.verbose);
        assert_matches!(
            cli.command,
            Command::Rm(rm::Args {
                keep_checkpoints: true,
                ..
            })
        );
        assert_matches!(
            parse(&["restore", "c1", "--pre"]).command,
            Command::Restore(restore::Args {
                pre_restore: true,
                ..
            })
        );
    }

    #[test]
    fn arguments_after_compose_are_not_ramet_options() {
        let cli = parse(&["-v", "compose", "exec", "-v", "db", "--help"]);
        assert!(cli.verbose);
        assert_eq!(forwarded(&cli), ["exec", "-v", "db", "--help"]);
    }

    #[test]
    fn ls_and_rm_are_ramet_s_and_compose_s_through_compose() {
        assert_matches!(
            parse(&["ls", "--json"]).command,
            Command::Ls(ls::Args { json: true })
        );
        assert_eq!(forwarded(&parse(&["compose", "ls"])), ["ls"]);
        assert_eq!(forwarded(&parse(&["compose", "rm", "-f"])), ["rm", "-f"]);
        assert!(check_compose_command(&os(&["rm", "-f"])).is_ok());
    }

    #[test]
    fn any_other_word_is_unknown() {
        assert_matches!(parse(&["up", "-d"]).command, Command::Unknown(args) if lossy(&args) == ["up", "-d"]);
    }

    #[test]
    fn a_bare_compose_command_gets_the_line_to_type() {
        assert_matches!(
            unknown_command(&os(&["up", "-d"])),
            Error::BareComposeCommand { word, command }
                if word == "up" && command == "ramet compose up -d"
        );
        assert_matches!(
            unknown_command(&os(&["exec", "db", "psql", "-c", "select 1"])),
            Error::BareComposeCommand { command, .. }
                if command == "ramet compose exec db psql -c 'select 1'"
        );
    }

    #[test]
    fn a_typo_gets_the_command_it_resembles() {
        for (typo, expected) in [
            ("checkpint", "ramet checkpoint"),
            ("restor", "ramet restore"),
            ("compse", "ramet compose"),
            ("logss", "ramet compose logs"),
            ("dwn", "ramet compose down"),
        ] {
            assert_matches!(
                unknown_command(&os(&[typo])),
                Error::UnknownCommand { suggestion: Some(suggestion), .. } if suggestion == expected,
                "{typo}"
            );
        }
        assert_matches!(
            unknown_command(&os(&["zzzqqq"])),
            Error::UnknownCommand {
                suggestion: None,
                ..
            }
        );
    }

    #[test]
    fn a_mistyped_compose_command_is_refused() {
        assert_matches!(
            check_compose_command(&os(&["dwn"])),
            Err(Error::UnknownCommand { suggestion: Some(suggestion), .. })
                if suggestion == "ramet compose down"
        );
        assert_matches!(
            check_compose_command(&os(&["stat"])),
            Err(Error::UnknownCommand { suggestion: Some(suggestion), .. })
                if suggestion == "ramet compose stats"
        );
    }

    #[test]
    fn a_ramet_command_given_to_compose_is_refused() {
        assert_matches!(
            check_compose_command(&os(&["restore", "c1"])),
            Err(Error::RametCommandInCompose { word, command, .. })
                if word == "restore" && command == "ramet restore c1"
        );
        assert_matches!(
            check_compose_command(&os(&["log"])),
            Err(Error::RametCommandInCompose { compose: Some(compose), .. })
                if compose == "ramet compose logs"
        );
    }

    #[test]
    fn an_unknown_compose_command_is_left_to_compose() {
        // A newer compose may know commands ramet does not.
        assert!(check_compose_command(&os(&["zzzqqq"])).is_ok());
        assert!(check_compose_command(&os(&["--profile", "tools", "run"])).is_ok());
        assert!(check_compose_command(&os(&["stats"])).is_ok());
    }

    #[test]
    fn command_names_match_what_the_user_typed() {
        let samples: [&[&str]; 16] = [
            &["setup"],
            &["doctor"],
            &["init"],
            &["new", "x"],
            &["ls"],
            &["checkpoint", "c1"],
            &["log"],
            &["restore", "c1"],
            &["rm", "x"],
            &["sync"],
            &["deinit"],
            &["df"],
            &["prune"],
            &["path"],
            &["prompt"],
            &["compose", "ps"],
        ];
        for sample in samples {
            assert_eq!(parse(sample).command.name(), sample[0]);
        }
        assert_eq!(
            samples.len(),
            ramet_commands().len(),
            "a command is missing above"
        );
        assert_eq!(parse(&["up", "-d"]).command.name(), "up");
    }

    #[test]
    fn project_settings_are_no_command_line_options() {
        // They live in `.ramet.json`, versioned with the project.
        for option in ["--profile", "--compose-file", "--copy"] {
            assert!(
                Cli::try_parse_from(["ramet", "init", option, "x"]).is_err(),
                "{option}"
            );
        }
        assert!(Cli::try_parse_from(["ramet", "new", "feat-a", "--copy", ".env"]).is_err());
    }

    #[test]
    fn setup_takes_a_size_with_its_unit() {
        let Command::Setup(args) = parse(&["setup", "--size", "20G"]).command else {
            panic!()
        };
        assert_eq!(args.size, Some(20 << 30));
        assert!(Cli::try_parse_from(["ramet", "setup", "--size", "20"]).is_err());
        assert_eq!(parse(&["setup"]).command.name(), "setup");
    }

    #[test]
    fn short_flags() {
        let Command::Prune(args) = parse(&["prune", "-y"]).command else {
            panic!()
        };
        assert!(args.yes);
        let Command::Sync(args) = parse(&["sync", "--from", "main", "-y"]).command else {
            panic!()
        };
        assert!(args.yes);
        assert_eq!(args.from.as_deref(), Some("main"));
        let Command::Restore(args) = parse(&["restore", "c1", "-y"]).command else {
            panic!()
        };
        assert!(args.yes);
        let Command::Checkpoint(args) = parse(&["checkpoint", "c1", "-m", "before"]).command else {
            panic!()
        };
        assert_eq!(args.message.as_deref(), Some("before"));
    }
}
