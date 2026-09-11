//! A running compose stack: how to address it and how to read its state.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::process::Cmd;

/// Everything docker compose needs to address the stack of one env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// The env's worktree, passed as `--project-directory` so that relative
    /// paths resolve as they do for the user.
    pub project_dir: PathBuf,
    /// The compose project name, `<project>-<env>`.
    pub name: String,
    /// The generated configuration.
    pub file: PathBuf,
}

impl Stack {
    /// `docker compose` addressing this stack, followed by `args`.
    pub fn command<I, S>(&self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Cmd::new("docker")
            .arg("compose")
            .arg("--project-directory")
            .arg(&self.project_dir)
            .args(["-p", &self.name])
            .arg("-f")
            .arg(&self.file)
            .args(args)
    }
}

/// One container, as `docker compose ps --format json` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Container {
    /// The service the container belongs to.
    #[serde(rename = "Service", default)]
    pub service: String,
    /// `running`, `exited`, `paused`…
    #[serde(rename = "State", default)]
    pub state: String,
    /// Exit code of a stopped container.
    #[serde(rename = "ExitCode", default)]
    pub exit_code: Option<i64>,
}

impl Container {
    fn is_running(&self) -> bool {
        self.state == "running"
    }

    /// A one-shot service (a bucket initialization, a migration) that ran and
    /// exited successfully: it did its job and is not missing.
    fn finished_successfully(&self) -> bool {
        self.state == "exited" && self.exit_code == Some(0)
    }
}

/// Parses `docker compose ps --format json`: a JSON array, or one object per
/// line depending on the compose version. Unreadable lines are skipped.
pub fn parse_containers(output: &str) -> Vec<Container> {
    let output = output.trim();
    if output.starts_with('[') {
        return serde_json::from_str(output).unwrap_or_default();
    }
    output
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect()
}

/// Whether a stack runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StackState {
    /// Every expected service runs.
    Up,
    /// Nothing runs.
    Down,
    /// Some services run, others are missing or crashed.
    Partial,
}

impl StackState {
    /// Derives the state from the stack's containers and the services its
    /// configuration declares (or, when unknown, the services of its containers).
    pub fn from_containers(containers: &[Container], declared: &BTreeSet<String>) -> Self {
        let running: BTreeSet<&str> = containers
            .iter()
            .filter(|c| c.is_running())
            .map(|c| c.service.as_str())
            .collect();
        let finished: BTreeSet<&str> = containers
            .iter()
            .filter(|c| c.finished_successfully())
            .map(|c| c.service.as_str())
            .collect();
        let expected: BTreeSet<&str> = if declared.is_empty() {
            containers.iter().map(|c| c.service.as_str()).collect()
        } else {
            declared.iter().map(String::as_str).collect()
        };
        if expected.is_empty() || running.is_empty() {
            Self::Down
        } else if expected
            .iter()
            .all(|service| running.contains(service) || finished.contains(service))
        {
            Self::Up
        } else {
            Self::Partial
        }
    }
}

impl fmt::Display for StackState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Partial => "partial",
        })
    }
}

/// Collects the `--profile` options passed to a `ramet compose` command, so that
/// the configuration is resolved with them too.
pub fn extract_profiles(args: &[String]) -> Vec<String> {
    let mut profiles = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--profile" {
            if let Some(profile) = args.next() {
                profiles.push(profile.clone());
            }
        } else if let Some(profile) = arg.strip_prefix("--profile=") {
            profiles.push(profile.to_owned());
        }
    }
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(ps_output: &str) -> StackState {
        let declared = BTreeSet::from(["web".to_owned(), "db".to_owned()]);
        StackState::from_containers(&parse_containers(ps_output), &declared)
    }

    #[test]
    fn everything_runs() {
        let output = r#"[{"Service":"web","State":"running"},{"Service":"db","State":"running"}]"#;
        assert_eq!(state(output), StackState::Up);
    }

    #[test]
    fn partially_running() {
        let output = r#"[{"Service":"web","State":"running"},{"Service":"db","State":"exited"}]"#;
        assert_eq!(state(output), StackState::Partial);
    }

    #[test]
    fn nothing_runs() {
        assert_eq!(
            state(r#"[{"Service":"web","State":"exited"}]"#),
            StackState::Down
        );
        assert_eq!(state("[]"), StackState::Down);
        assert_eq!(state(""), StackState::Down);
    }

    #[test]
    fn a_job_that_finished_cleanly_does_not_make_the_stack_partial() {
        // Seen on a real project: `minio-init` made a healthy stack read
        // "partial" at every `ramet ls`.
        let output = r#"[{"Service":"web","State":"running","ExitCode":0},
                         {"Service":"db","State":"exited","ExitCode":0}]"#;
        assert_eq!(state(output), StackState::Up);
    }

    #[test]
    fn a_crashed_service_makes_the_stack_partial() {
        let output = r#"[{"Service":"web","State":"running","ExitCode":0},
                         {"Service":"db","State":"exited","ExitCode":1}]"#;
        assert_eq!(state(output), StackState::Partial);
    }

    #[test]
    fn one_object_per_line() {
        let output = "{\"Service\":\"web\",\"State\":\"running\"}\n{\"Service\":\"db\",\"State\":\"running\"}\n";
        assert_eq!(state(output), StackState::Up);
    }

    #[test]
    fn falls_back_to_the_services_of_the_containers() {
        let containers = parse_containers(r#"[{"Service":"web","State":"running"}]"#);
        assert_eq!(
            StackState::from_containers(&containers, &BTreeSet::new()),
            StackState::Up
        );
    }

    #[test]
    fn serializes_in_lowercase() {
        assert_eq!(
            serde_json::to_string(&StackState::Partial).unwrap(),
            "\"partial\""
        );
    }

    #[test]
    fn extracts_profiles_in_both_forms() {
        let args = |list: &[&str]| list.iter().map(|&a| a.to_owned()).collect::<Vec<_>>();
        assert_eq!(
            extract_profiles(&args(&["-d", "--profile", "debug"])),
            vec!["debug"]
        );
        assert_eq!(extract_profiles(&args(&["--profile=debug"])), vec!["debug"]);
        assert_eq!(
            extract_profiles(&args(&["--profile", "a", "--profile=b"])),
            vec!["a", "b"]
        );
        assert!(extract_profiles(&args(&["-d", "--build"])).is_empty());
    }

    #[test]
    fn command_addresses_the_stack() {
        let stack = Stack {
            project_dir: PathBuf::from("/code/app"),
            name: "example-feat-a".to_owned(),
            file: PathBuf::from("/srv/ramet/example/feat-a/ramet.compose.json"),
        };
        assert_eq!(
            stack.command(["up", "-d"]).argv(),
            [
                "docker",
                "compose",
                "--project-directory",
                "/code/app",
                "-p",
                "example-feat-a",
                "-f",
                "/srv/ramet/example/feat-a/ramet.compose.json",
                "up",
                "-d",
            ]
        );
    }
}
