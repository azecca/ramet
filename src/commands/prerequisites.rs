//! The tools ramet cannot work without, checked by `doctor` and `setup`.

use crate::context::Context;
use crate::process::{Cmd, RunnerExt};

/// Oldest git with worktrees.
const MIN_GIT_VERSION: (u32, u32) = (2, 5);

/// One prerequisite, checked.
pub(crate) struct Check {
    /// Short name of the prerequisite.
    pub(crate) name: &'static str,
    /// Whether the data volume cannot be prepared without it.
    pub(crate) for_volume: bool,
    /// Whether the prerequisite is met.
    pub(crate) met: bool,
    /// What was found, or what is wrong.
    pub(crate) message: String,
    /// What to do about it, when it is not met and the message does not say.
    pub(crate) hint: Option<&'static str>,
}

impl Check {
    fn new(name: &'static str, met: bool, message: impl Into<String>) -> Self {
        Self {
            name,
            for_volume: false,
            met,
            message: message.into(),
            hint: None,
        }
    }

    fn for_volume(mut self) -> Self {
        self.for_volume = true;
        self
    }
}

/// Checks every prerequisite, in the order a user would install them.
pub(crate) fn check(ctx: &Context) -> Vec<Check> {
    let mut checks = vec![util_linux(ctx), btrfs_progs(ctx), git(ctx)];
    checks.extend(docker(ctx));
    checks
}

/// ramet shells out to these to find, mount and describe the data volume.
fn util_linux(ctx: &Context) -> Check {
    let missing: Vec<&str> = ["findmnt", "mount"]
        .into_iter()
        .filter(|tool| ctx.host().find_program(tool).is_none())
        .collect();
    let message = if missing.is_empty() {
        "util-linux: findmnt, mount".to_owned()
    } else {
        format!("{} not found: install util-linux", missing.join(", "))
    };
    Check::new("util-linux", missing.is_empty(), message).for_volume()
}

fn btrfs_progs(ctx: &Context) -> Check {
    if ctx.host().find_program("btrfs").is_none() {
        return Check::new("btrfs-progs", false, "btrfs not found: install btrfs-progs")
            .for_volume();
    }
    let output = ctx
        .runner()
        .run_unchecked(&Cmd::new("btrfs").arg("--version"));
    // Recent versions add a line of build features after the version.
    let version = output
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown version");
    Check::new("btrfs-progs", true, format!("btrfs-progs: {version}")).for_volume()
}

fn git(ctx: &Context) -> Check {
    let output = ctx
        .runner()
        .run_unchecked(&Cmd::new("git").arg("--version"));
    if !output.success() {
        return Check::new("git", false, "git not found: install git");
    }
    let version = output.stdout.split_whitespace().last().unwrap_or("?");
    if parse_version(version) >= MIN_GIT_VERSION {
        Check::new("git", true, format!("git {version}"))
    } else {
        Check::new(
            "git",
            false,
            format!("git {version} is older than 2.5 (worktrees)"),
        )
    }
}

/// The docker client, its compose plugin and the daemon: each depends on the
/// previous one, so checking stops at the first that is missing.
fn docker(ctx: &Context) -> Vec<Check> {
    if ctx.host().find_program("docker").is_none() {
        return vec![Check::new(
            "docker",
            false,
            "docker not found: install Docker Engine",
        )];
    }
    let runner = ctx.runner();
    let compose = runner.run_unchecked(&Cmd::new("docker").args(["compose", "version", "--short"]));
    let compose = if compose.success() {
        Check::new(
            "docker compose",
            true,
            format!("docker compose {}", compose.stdout_trimmed()),
        )
    } else {
        Check::new(
            "docker compose",
            false,
            "docker compose v2 missing (`docker compose version` fails)",
        )
    };
    let info =
        runner.run_unchecked(&Cmd::new("docker").args(["info", "--format", "{{.ServerVersion}}"]));
    let daemon = if info.success() {
        Check::new(
            "docker engine",
            true,
            format!("docker engine {}, daemon reachable", info.stdout_trimmed()),
        )
    } else {
        let mut check = Check::new(
            "docker engine",
            false,
            format!(
                "the docker daemon cannot be reached: {}",
                info.last_error_line().unwrap_or("?")
            ),
        );
        if info.stderr.to_lowercase().contains("permission denied") {
            check.hint = Some("the user must belong to the docker group");
        }
        check
    };
    vec![compose, daemon]
}

/// `major.minor` of a dotted version; missing parts read as 0.
fn parse_version(version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_versions() {
        assert_eq!(parse_version("2.55.0"), (2, 55));
        assert_eq!(parse_version("2.5"), (2, 5));
        assert!(parse_version("2.4.11") < MIN_GIT_VERSION);
        assert!(parse_version("2.39.2.windows.1") >= MIN_GIT_VERSION);
    }
}
