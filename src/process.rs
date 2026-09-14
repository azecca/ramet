//! Execution of external programs.
//!
//! Every external call made by ramet (btrfs, git, docker, findmnt, mount) goes
//! through a [`Runner`]. A single gateway keeps `--verbose` logging exhaustive
//! and lets tests substitute a scripted runner for the real system.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{Error, Result};
use crate::host::locate_program;
use crate::ui::Style;

/// Exit code a shell reports for a program it cannot find.
const NOT_FOUND_CODE: i32 = 127;
/// Exit code a shell reports for a program it found but could not start.
const NOT_EXECUTABLE_CODE: i32 = 126;
/// Offset a shell adds to a signal number to report a signal-terminated child.
const SIGNAL_CODE_OFFSET: i32 = 128;

/// What happens to the standard streams of a child process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Standard output and error are captured into [`Output`]; standard input
    /// carries the command's [input](Cmd::input), or is closed so that a
    /// captured command can never wait on the user.
    #[default]
    Captured,
    /// All three standard streams are shared with ramet: the user sees the
    /// program's output live and can interact with it.
    Inherited,
}

/// An external command line, built fluently and executed by a [`Runner`].
#[derive(Clone, Debug)]
pub struct Cmd {
    program: OsString,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    envs: BTreeMap<OsString, OsString>,
    removed_envs: BTreeSet<OsString>,
    input: Option<String>,
    mode: OutputMode,
}

impl Cmd {
    /// Starts a command line for `program`, looked up in `PATH`, then in
    /// `/usr/sbin` and `/sbin`, which a regular user's `PATH` may lack.
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            current_dir: None,
            envs: BTreeMap::new(),
            removed_envs: BTreeSet::new(),
            input: None,
            mode: OutputMode::default(),
        }
    }

    /// Appends one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    /// Runs the command from `dir` instead of ramet's working directory.
    #[must_use]
    pub fn current_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.current_dir = Some(dir.as_ref().to_owned());
        self
    }

    /// Sets an environment variable on top of the inherited environment.
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.removed_envs.remove(key.as_ref());
        self.envs
            .insert(key.as_ref().to_owned(), value.as_ref().to_owned());
        self
    }

    /// Keeps an environment variable of ramet's own environment from the
    /// command.
    #[must_use]
    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.envs.remove(key.as_ref());
        self.removed_envs.insert(key.as_ref().to_owned());
        self
    }

    /// Feeds `text` to the standard input of the command. Only a captured
    /// command has an input of its own: an inherited one reads the user's.
    #[must_use]
    pub fn input(mut self, text: impl Into<String>) -> Self {
        self.input = Some(text.into());
        self
    }

    /// Shares the standard streams with ramet instead of capturing them.
    #[must_use]
    pub fn inherit_output(mut self) -> Self {
        self.mode = OutputMode::Inherited;
        self
    }

    /// The program to run.
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// The arguments, without the program.
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// The working directory, if one was set.
    pub fn working_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    /// The environment variables set on top of the inherited environment.
    pub fn env_vars(&self) -> &BTreeMap<OsString, OsString> {
        &self.envs
    }

    /// The environment variables kept from the command.
    pub fn removed_env_vars(&self) -> &BTreeSet<OsString> {
        &self.removed_envs
    }

    /// What the command receives on its standard input, if anything.
    pub fn input_text(&self) -> Option<&str> {
        self.input.as_deref()
    }

    /// How the standard streams are handled.
    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    /// The program followed by its arguments, as (lossy) UTF-8 strings.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|part| part.to_string_lossy().into_owned())
            .collect()
    }

    /// The value of an environment variable set on this command, if any.
    pub fn env_var(&self, key: &str) -> Option<String> {
        self.envs
            .get(OsStr::new(key))
            .map(|value| value.to_string_lossy().into_owned())
    }
}

impl fmt::Display for Cmd {
    /// Formats the command line as it could be pasted into a shell.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quoted: Vec<String> = self.argv().iter().map(|part| shell_quote(part)).collect();
        f.write_str(&quoted.join(" "))
    }
}

/// The result of a finished command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// Exit code; `128 + n` for a child killed by signal `n`, like a shell.
    pub code: i32,
    /// Captured standard output; empty for inherited streams.
    pub stdout: String,
    /// Captured standard error; empty for inherited streams.
    pub stderr: String,
}

impl Output {
    /// A successful result carrying `stdout`.
    pub fn success_with(stdout: impl Into<String>) -> Self {
        Self {
            code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A failed result with the given exit code and error output.
    pub fn failure(code: i32, stderr: impl Into<String>) -> Self {
        Self {
            code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    /// Whether the command exited with code 0.
    pub fn success(&self) -> bool {
        self.code == 0
    }

    /// Standard output without surrounding whitespace.
    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }

    /// The last non-empty line of standard error, usually the one that explains a failure.
    pub fn last_error_line(&self) -> Option<&str> {
        self.stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
    }

    /// Why the command failed: its standard error, or its exit code when silent.
    pub fn failure_detail(&self) -> String {
        let stderr = self.stderr.trim();
        if stderr.is_empty() {
            format!("exit code {}", self.code)
        } else {
            stderr.to_owned()
        }
    }
}

/// Executes external commands.
pub trait Runner {
    /// Runs `cmd` to completion.
    ///
    /// Fails only when the program cannot be started at all; a program that
    /// runs and exits with a non-zero code is reported through [`Output::code`].
    fn run(&self, cmd: &Cmd) -> io::Result<Output>;
}

/// Convenience wrappers around [`Runner::run`] shared by every runner.
pub trait RunnerExt: Runner {
    /// Runs `cmd` and requires it to succeed.
    fn run_checked(&self, cmd: &Cmd) -> Result<Output> {
        match self.run(cmd) {
            Ok(output) if output.success() => Ok(output),
            Ok(output) => Err(Error::CommandFailed {
                command: cmd.to_string(),
                detail: output.failure_detail(),
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(Error::CommandNotFound {
                program: cmd.program().to_string_lossy().into_owned(),
            }),
            Err(err) => Err(Error::CommandFailed {
                command: cmd.to_string(),
                detail: err.to_string(),
            }),
        }
    }

    /// Runs `cmd` and reports any failure in the returned [`Output`].
    ///
    /// A program that cannot be started yields exit code 127 (not found) or
    /// 126 (not executable), so that callers probing the system, such as
    /// `ramet doctor`, survive the very environment they diagnose.
    fn run_unchecked(&self, cmd: &Cmd) -> Output {
        self.run(cmd).unwrap_or_else(|err| {
            let code = if err.kind() == io::ErrorKind::NotFound {
                NOT_FOUND_CODE
            } else {
                NOT_EXECUTABLE_CODE
            };
            Output::failure(code, format!("{}: {err}", cmd.program().to_string_lossy()))
        })
    }
}

impl<R: Runner + ?Sized> RunnerExt for R {}

/// Runs commands on the host system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner {
    verbose: bool,
    style: Style,
}

impl SystemRunner {
    /// A runner that logs every command to standard error when `verbose` is set.
    pub fn new(verbose: bool, style: Style) -> Self {
        Self { verbose, style }
    }

    fn log(self, cmd: &Cmd) {
        let mut env_prefix = String::new();
        if !cmd.removed_env_vars().is_empty() {
            env_prefix.push_str("env ");
            for key in cmd.removed_env_vars() {
                let _ = write!(env_prefix, "-u {} ", key.to_string_lossy());
            }
        }
        for (key, value) in cmd.env_vars() {
            let _ = write!(
                env_prefix,
                "{}={} ",
                key.to_string_lossy(),
                shell_quote(&value.to_string_lossy())
            );
        }
        let location = cmd
            .working_dir()
            .map(|dir| format!("  {}", self.style.dim(format!("in {}", dir.display()))))
            .unwrap_or_default();
        let line = self.style.dim(format!("+ {env_prefix}{cmd}"));
        // Logging must never abort the command it describes.
        let _ = writeln!(io::stderr(), "{line}{location}");
    }
}

impl Runner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        if self.verbose {
            self.log(cmd);
        }
        let mut command = Command::new(program_to_start(cmd.program(), locate_program));
        command.args(cmd.arguments()).envs(cmd.env_vars());
        for key in cmd.removed_env_vars() {
            command.env_remove(key);
        }
        if let Some(dir) = cmd.working_dir() {
            command.current_dir(dir);
        }
        match cmd.mode() {
            OutputMode::Captured => {
                let output = match cmd.input_text() {
                    None => command.stdin(Stdio::null()).output()?,
                    Some(input) => run_with_input(&mut command, input)?,
                };
                Ok(Output {
                    code: exit_code(output.status),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
            OutputMode::Inherited => {
                debug_assert!(
                    cmd.input_text().is_none(),
                    "an inherited command reads the user's input"
                );
                let status = command.status()?;
                Ok(Output {
                    code: exit_code(status),
                    ..Output::default()
                })
            }
        }
    }
}

/// The file to start for `program`: a bare name is looked up with `locate`,
/// and kept as it is when that finds nothing.
fn program_to_start(program: &OsStr, locate: impl Fn(&str) -> Option<PathBuf>) -> Cow<'_, OsStr> {
    let bare = !program.as_bytes().contains(&b'/');
    program
        .to_str()
        .filter(|_| bare)
        .and_then(locate)
        .map_or(Cow::Borrowed(program), |path| Cow::Owned(path.into()))
}

/// Runs `command` with `input` on its standard input, capturing its output.
///
/// The input is written from a separate thread: a child that fills its output
/// pipe before reading all of its input would otherwise wait on ramet forever,
/// while ramet waits on it.
fn run_with_input(command: &mut Command, input: &str) -> io::Result<std::process::Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("standard input is piped");
    std::thread::scope(|scope| {
        // A child that exits without reading its input closes the pipe: that
        // is its own business, and its exit code tells the story.
        scope.spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        child.wait_with_output()
    })
}

/// Converts an exit status into the code a shell would report.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| SIGNAL_CODE_OFFSET + status.signal().unwrap_or(0))
}

/// Quotes `word` for a POSIX shell, leaving it bare when that is unambiguous.
pub fn shell_quote(word: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c);
    if !word.is_empty() && word.chars().all(is_safe) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r#"'"'"'"#))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn a_bare_program_is_started_from_where_it_was_found() {
        let losetup = PathBuf::from("/usr/sbin/losetup");
        let found = |name: &str| (name == "losetup").then(|| losetup.clone());

        assert_eq!(
            program_to_start(OsStr::new("losetup"), found),
            losetup.as_os_str()
        );
        // Not found anywhere: the name is kept, for the error to report it.
        assert_eq!(
            program_to_start(OsStr::new("absent"), found),
            OsStr::new("absent")
        );
        // A path is taken as it is.
        let path = OsStr::new("./bin/losetup");
        assert_eq!(program_to_start(path, |_| panic!("not looked up")), path);
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(shell_quote("docker"), "docker");
        assert_eq!(shell_quote("/srv/ramet/app-main"), "/srv/ramet/app-main");
        assert_eq!(shell_quote("{{.Name}}"), "'{{.Name}}'");
        assert_eq!(shell_quote("it's"), r#"'it'"'"'s'"#);
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn displays_a_pasteable_command_line() {
        let cmd = Cmd::new("docker").args(["volume", "ls", "--format", "{{.Name}}"]);
        assert_eq!(cmd.to_string(), "docker volume ls --format '{{.Name}}'");
    }

    #[test]
    fn failure_detail_falls_back_to_the_exit_code() {
        assert_eq!(Output::failure(3, "  \n").failure_detail(), "exit code 3");
        assert_eq!(Output::failure(1, "boom\n").failure_detail(), "boom");
    }

    #[test]
    fn last_error_line_skips_trailing_blank_lines() {
        let output = Output::failure(1, "first\nsecond\n\n");
        assert_eq!(output.last_error_line(), Some("second"));
        assert_eq!(Output::default().last_error_line(), None);
    }

    #[test]
    fn a_missing_program_is_an_exploitable_failure() {
        let runner = SystemRunner::default();
        let cmd = Cmd::new("/nonexistent/ramet-test-binary");

        let output = runner.run_unchecked(&cmd);
        assert_eq!(output.code, NOT_FOUND_CODE);
        assert!(
            output.stdout.is_empty(),
            "callers trim stdout without checking"
        );

        let err = runner.run_checked(&cmd).unwrap_err();
        assert_matches!(err, Error::CommandNotFound { .. });
    }

    #[test]
    fn captures_output_and_exit_code() {
        let runner = SystemRunner::default();
        let output = runner
            .run_checked(&Cmd::new("sh").args(["-c", "printf out; printf err >&2"]))
            .unwrap();
        assert_eq!(
            (output.stdout.as_str(), output.stderr.as_str()),
            ("out", "err")
        );

        let failed = runner.run_unchecked(&Cmd::new("sh").args(["-c", "exit 4"]));
        assert_eq!(failed.code, 4);
    }

    #[test]
    fn feeds_the_input_to_a_captured_command() {
        let output = SystemRunner::default()
            .run_checked(&Cmd::new("cat").input("a line\n"))
            .unwrap();
        assert_eq!(output.stdout, "a line\n");
    }

    #[test]
    fn passes_environment_and_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let output = SystemRunner::default()
            .run_checked(
                &Cmd::new("sh")
                    .args(["-c", "printf '%s %s' \"$RAMET_TEST\" \"$PWD\""])
                    .env("RAMET_TEST", "value")
                    .current_dir(dir.path()),
            )
            .unwrap();
        assert_eq!(output.stdout, format!("value {}", dir.path().display()));
    }

    #[test]
    fn a_removed_variable_is_not_inherited() {
        // `HOME` is set in any test run: only the removal can take it away.
        let cmd = Cmd::new("sh")
            .args(["-c", "printf '%s' \"${HOME-unset}\""])
            .env("HOME", "/tmp")
            .env_remove("HOME");
        assert_eq!(cmd.env_var("HOME"), None);
        let output = SystemRunner::default().run_checked(&cmd).unwrap();
        assert_eq!(output.stdout, "unset");

        let set_again = Cmd::new("true").env_remove("HOME").env("HOME", "/tmp");
        assert!(set_again.removed_env_vars().is_empty());
        assert_eq!(set_again.env_var("HOME").as_deref(), Some("/tmp"));
    }
}
