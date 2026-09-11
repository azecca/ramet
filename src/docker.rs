//! Docker volumes, as `ramet init` finds and copies them.

use std::path::Path;

use crate::error::Result;
use crate::process::{Cmd, Runner, RunnerExt};

/// Image used for throwaway containers that read or copy volumes.
const HELPER_IMAGE: &str = "alpine";

/// Docker operations through the `docker` command line tool.
pub struct Docker<'a> {
    runner: &'a dyn Runner,
}

impl<'a> Docker<'a> {
    /// Runs `docker` through `runner`.
    pub fn new(runner: &'a dyn Runner) -> Self {
        Self { runner }
    }

    /// Whether a docker volume named `name` exists.
    pub fn volume_exists(&self, name: &str) -> bool {
        self.runner
            .run_unchecked(&Cmd::new("docker").args(["volume", "inspect", name]))
            .success()
    }

    /// Names of every docker volume on the machine.
    pub fn volume_names(&self) -> Vec<String> {
        let output = self.runner.run_unchecked(&Cmd::new("docker").args([
            "volume",
            "ls",
            "--format",
            "{{.Name}}",
        ]));
        if !output.success() {
            return Vec::new();
        }
        output
            .stdout
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    /// Bytes occupied by the volume `name`, or 0 when it cannot be measured.
    ///
    /// A missing volume measures 0 without starting anything: `docker run -v`
    /// would create it, and an empty volume left behind would later pass for
    /// a migration source.
    pub fn volume_size(&self, name: &str) -> u64 {
        if !self.volume_exists(name) {
            return 0;
        }
        // `du -sk` rather than `-sb`: the form busybox documents.
        let mount = format!("{name}:/v");
        let cmd =
            Cmd::new("docker").args(["run", "--rm", "-v", &mount, HELPER_IMAGE, "du", "-sk", "/v"]);
        let output = self.runner.run_unchecked(&cmd);
        if !output.success() {
            return 0;
        }
        output
            .stdout
            .split_whitespace()
            .next()
            .and_then(|kib| kib.parse::<u64>().ok())
            .map_or(0, |kib| kib.saturating_mul(1024))
    }

    /// Copies the content of the volume `source` into `destination`,
    /// preserving owners, modes and timestamps.
    pub fn copy_volume(&self, source: &str, destination: &Path) -> Result<()> {
        let mut target = destination.as_os_str().to_owned();
        target.push(":/to");
        let cmd = Cmd::new("docker")
            .args(["run", "--rm", "-v", &format!("{source}:/from"), "-v"])
            .arg(target)
            .args([HELPER_IMAGE, "cp", "-a", "/from/.", "/to/"]);
        self.runner.run_checked(&cmd).map(drop)
    }
}
