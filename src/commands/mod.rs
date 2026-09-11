//! One module per command. Each defines its arguments and a `run` function.

pub mod checkpoint;
pub mod deinit;
pub mod df;
pub mod doctor;
pub mod init;
pub mod log;
pub mod ls;
pub mod new;
pub mod passthrough;
pub mod path;
pub mod prompt;
pub mod prune;
pub mod restore;
pub mod rm;
pub mod setup;
pub mod sync;

mod prerequisites;
mod support;

/// How a command ended, when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The command did what it was asked.
    Done,
    /// The user declined a confirmation; nothing was changed.
    Aborted,
    /// The command ends with this exit code: one forwarded from another
    /// program, or 1 when `doctor` found errors or `setup` could not finish.
    Exit(u8),
}

impl Outcome {
    /// The process exit code.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Aborted => 1,
            Self::Exit(code) => code,
        }
    }
}
