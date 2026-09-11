//! Docker compose: resolving a project's configuration and driving stacks.
//!
//! The generated configuration is a derived cache, never edited by hand and
//! regenerated at every command. Any change the developer makes to the
//! project's compose files is thus picked up immediately, and compose does
//! its usual reconciliation.

pub mod config;
pub mod discovery;
pub mod stack;

use std::path::Path;

pub use config::{ComposeConfig, DeclaredVolumes};
pub use stack::{Container, Stack, StackState};

use crate::error::{Error, Result};
use crate::host::Host;
use crate::process::{Cmd, Runner, RunnerExt};

/// Number of error lines of `docker compose config` quoted when it fails.
const CONFIG_ERROR_LINES: usize = 4;

/// Docker compose operations that are not tied to one env.
pub struct Compose<'a> {
    runner: &'a dyn Runner,
    host: &'a dyn Host,
}

impl<'a> Compose<'a> {
    /// Runs `docker compose` through `runner`; `host` provides the shell's
    /// `COMPOSE_FILE`.
    pub fn new(runner: &'a dyn Runner, host: &'a dyn Host) -> Self {
        Self { runner, host }
    }

    /// The configuration compose resolves for `worktree`, exactly what the
    /// developer gets with `docker compose up`, or with `docker compose -f
    /// <file> -f <file> up` for a project declaring its `files`.
    ///
    /// No `-f` is ever passed. `profiles` are replayed with `--profile`, and
    /// declared `files` are handed over through `COMPOSE_FILE`, compose's own
    /// mechanism, with the project directory `-f` would give: see
    /// [`discovery::project_directory`].
    pub fn resolve(
        &self,
        worktree: &Path,
        profiles: &[String],
        files: &[String],
    ) -> Result<ComposeConfig> {
        let files = discovery::normalize_compose_files(worktree, files)?;
        if files.is_empty() {
            discovery::ensure_discoverable(worktree, self.host.var("COMPOSE_FILE").as_deref())?;
        }
        let mut cmd = Cmd::new("docker")
            .arg("compose")
            .arg("--project-directory")
            .arg(discovery::project_directory(worktree, &files));
        for profile in profiles {
            cmd = cmd.args(["--profile", profile]);
        }
        cmd = cmd
            .args(["config", "--format", "json"])
            .current_dir(worktree);
        if !files.is_empty() {
            cmd = cmd
                .env(
                    "COMPOSE_FILE",
                    files.join(discovery::COMPOSE_PATH_SEPARATOR),
                )
                .env("COMPOSE_PATH_SEPARATOR", discovery::COMPOSE_PATH_SEPARATOR);
        }
        let output = self.runner.run_unchecked(&cmd);
        if !output.success() {
            let lines: Vec<&str> = output.stderr.trim().lines().collect();
            let tail = &lines[lines.len().saturating_sub(CONFIG_ERROR_LINES)..];
            let detail = if tail.is_empty() {
                "(no message)".to_owned()
            } else {
                tail.join("\n")
            };
            return Err(Error::ComposeConfigFailed {
                worktree: worktree.to_owned(),
                detail,
            });
        }
        ComposeConfig::from_json(&output.stdout)
    }

    /// Containers of the compose project `name`, running or not.
    ///
    /// Read with `--format json`: the text output of `ps` is never parsed.
    pub fn containers(&self, name: &str) -> Vec<Container> {
        let cmd = Cmd::new("docker").args(["compose", "-p", name, "ps", "-a", "--format", "json"]);
        let output = self.runner.run_unchecked(&cmd);
        if output.success() {
            stack::parse_containers(&output.stdout)
        } else {
            Vec::new()
        }
    }

    /// Stops the compose project `name`, designated by its name alone.
    ///
    /// A bare `docker compose down` would resolve the files of the current
    /// directory and could target an unrelated project. Without `-v`: the
    /// volumes are kept.
    pub fn down_project(&self, name: &str) -> Result<()> {
        let cmd = Cmd::new("docker").args(["compose", "-p", name, "down", "--remove-orphans"]);
        self.runner.run_checked(&cmd).map(drop)
    }
}
