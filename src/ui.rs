//! Terminal presentation: colours, status markers and yes/no prompts.
//!
//! Output streams and the input stream are injected so that commands can be
//! exercised in tests without a terminal.

use std::cell::RefCell;
use std::fmt::Display;
use std::io::{self, BufRead, IsTerminal, Write};

use crate::error::{Error, Result};

/// ANSI styling, enabled only when standard output is a terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    enabled: bool,
}

impl Style {
    /// Styling that emits no escape sequences.
    pub const fn plain() -> Self {
        Self { enabled: false }
    }

    /// Styling that always emits escape sequences.
    pub const fn colored() -> Self {
        Self { enabled: true }
    }

    /// Colours when standard output is a terminal, plain text otherwise.
    pub fn detect() -> Self {
        Self {
            enabled: io::stdout().is_terminal(),
        }
    }

    fn paint(self, code: &str, text: impl Display) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    /// Bold text.
    pub fn bold(self, text: impl Display) -> String {
        self.paint("1", text)
    }

    /// Red text, for errors.
    pub fn red(self, text: impl Display) -> String {
        self.paint("31", text)
    }

    /// Green text, for success.
    pub fn green(self, text: impl Display) -> String {
        self.paint("32", text)
    }

    /// Yellow text, for warnings.
    pub fn yellow(self, text: impl Display) -> String {
        self.paint("33", text)
    }

    /// Cyan text, for information.
    pub fn cyan(self, text: impl Display) -> String {
        self.paint("36", text)
    }

    /// Dimmed text, for secondary details.
    pub fn dim(self, text: impl Display) -> String {
        self.paint("2", text)
    }
}

/// The streams a [`Ui`] reads from and writes to, and what they are attached to.
pub struct Streams {
    /// Destination of regular output.
    pub stdout: Box<dyn Write>,
    /// Destination of diagnostics, warnings and errors.
    pub stderr: Box<dyn Write>,
    /// Source of answers to prompts.
    pub stdin: Box<dyn BufRead>,
    /// Whether standard input is an interactive terminal.
    pub stdin_is_terminal: bool,
    /// Whether standard error is an interactive terminal.
    pub stderr_is_terminal: bool,
}

/// Where ramet talks to its user.
///
/// Write errors are deliberately ignored: a closed pipe (`ramet ls | head`)
/// must not turn a successful command into a failure.
pub struct Ui {
    style: Style,
    stdout: RefCell<Box<dyn Write>>,
    stderr: RefCell<Box<dyn Write>>,
    stdin: RefCell<Box<dyn BufRead>>,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
}

impl Ui {
    /// A user interface over the process's standard streams.
    pub fn stdio() -> Self {
        Self::new(
            Style::detect(),
            Streams {
                stdout: Box::new(io::stdout()),
                stderr: Box::new(io::stderr()),
                stdin: Box::new(io::stdin().lock()),
                stdin_is_terminal: io::stdin().is_terminal(),
                stderr_is_terminal: io::stderr().is_terminal(),
            },
        )
    }

    /// A user interface over arbitrary streams.
    pub fn new(style: Style, streams: Streams) -> Self {
        Self {
            style,
            stdout: RefCell::new(streams.stdout),
            stderr: RefCell::new(streams.stderr),
            stdin: RefCell::new(streams.stdin),
            stdin_is_terminal: streams.stdin_is_terminal,
            stderr_is_terminal: streams.stderr_is_terminal,
        }
    }

    /// The styling applied to every message.
    pub fn style(&self) -> Style {
        self.style
    }

    /// Whether standard input is an interactive terminal.
    pub fn stdin_is_terminal(&self) -> bool {
        self.stdin_is_terminal
    }

    /// Whether standard error is an interactive terminal.
    pub fn stderr_is_terminal(&self) -> bool {
        self.stderr_is_terminal
    }

    /// Writes one line to standard output.
    pub fn out(&self, line: impl Display) {
        let _ = writeln!(self.stdout.borrow_mut(), "{line}");
    }

    /// Writes raw text to standard output, without adding a newline.
    pub fn out_raw(&self, text: &str) {
        let mut stdout = self.stdout.borrow_mut();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }

    /// Writes an empty line to standard output.
    pub fn blank(&self) {
        self.out("");
    }

    /// Writes one line to standard error.
    pub fn err(&self, line: impl Display) {
        let _ = writeln!(self.stderr.borrow_mut(), "{line}");
    }

    /// Reports a completed step: `  ✓ message`.
    pub fn ok(&self, message: impl Display) {
        self.out(format!("  {} {message}", self.style.green("✓")));
    }

    /// Reports a neutral fact: `  · message`.
    pub fn note(&self, message: impl Display) {
        self.out(format!("  {} {message}", self.style.cyan("·")));
    }

    /// Reports something that deserves attention: `  ! message`.
    pub fn warn(&self, message: impl Display) {
        self.out(format!("  {} {message}", self.style.yellow("!")));
    }

    /// Asks a yes/no question.
    ///
    /// `assume_yes` answers yes without asking. Without a terminal, a question
    /// whose default is yes takes its default, since nothing destructive
    /// hinges on it; any other question requires `--yes`.
    pub fn confirm(&self, question: &str, assume_yes: bool, default: bool) -> Result<bool> {
        if assume_yes {
            return Ok(true);
        }
        if !self.stdin_is_terminal {
            return if default {
                Ok(true)
            } else {
                Err(Error::ConfirmationRequired {
                    question: question.trim().to_owned(),
                })
            };
        }
        let choices = if default { "[Y/n]" } else { "[y/N]" };
        self.out_raw(&format!("{question} {choices} "));
        let mut answer = String::new();
        // End of input counts as an empty answer, hence the default.
        let _ = self.stdin.borrow_mut().read_line(&mut answer);
        let answer = answer.trim().to_lowercase();
        Ok(match answer.as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::io::Cursor;

    fn ui_with_input(input: &str, terminal: bool) -> Ui {
        Ui::new(
            Style::plain(),
            Streams {
                stdout: Box::new(io::sink()),
                stderr: Box::new(io::sink()),
                stdin: Box::new(Cursor::new(input.to_owned())),
                stdin_is_terminal: terminal,
                stderr_is_terminal: false,
            },
        )
    }

    #[test]
    fn assume_yes_skips_the_question() {
        assert!(
            ui_with_input("", false)
                .confirm("continue?", true, false)
                .unwrap()
        );
    }

    #[test]
    fn without_a_terminal_a_risky_question_requires_yes() {
        let err = ui_with_input("", false)
            .confirm("continue?", false, false)
            .unwrap_err();
        assert_matches!(err, Error::ConfirmationRequired { .. });
        assert!(err.hint().unwrap().contains("--yes"));
    }

    #[test]
    fn without_a_terminal_a_default_yes_question_takes_its_default() {
        assert!(
            ui_with_input("", false)
                .confirm("freeze?", false, true)
                .unwrap()
        );
    }

    #[test]
    fn enter_takes_the_default() {
        assert!(
            ui_with_input("\n", true)
                .confirm("freeze?", false, true)
                .unwrap()
        );
        assert!(
            !ui_with_input("\n", true)
                .confirm("delete?", false, false)
                .unwrap()
        );
    }

    #[test]
    fn end_of_input_takes_the_default() {
        assert!(
            !ui_with_input("", true)
                .confirm("delete?", false, false)
                .unwrap()
        );
    }

    #[test]
    fn accepted_answers() {
        for (answer, expected) in [
            ("y", true),
            ("yes", true),
            ("YES", true),
            ("n", false),
            ("no", false),
            ("maybe", false),
        ] {
            let ui = ui_with_input(&format!("{answer}\n"), true);
            assert_eq!(
                ui.confirm("q?", false, false).unwrap(),
                expected,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn a_default_yes_question_can_be_declined() {
        assert!(
            !ui_with_input("n\n", true)
                .confirm("freeze?", false, true)
                .unwrap()
        );
    }

    #[test]
    fn plain_style_emits_no_escape_sequences() {
        assert_eq!(Style::plain().red("x"), "x");
        assert_eq!(Style::colored().red("x"), "\x1b[31mx\x1b[0m");
    }
}
