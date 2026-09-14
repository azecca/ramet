//! Running a parsed command: the checks every command goes through, dispatch,
//! and how errors reach the user.

use crate::cli::{Command, unknown_command};
use crate::commands::{
    Outcome, checkpoint, deinit, df, doctor, init, log, ls, new, passthrough, path, prompt, prune,
    restore, rm, setup, sync,
};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::host::Host;
use crate::interrupt;
use crate::ui::Ui;

/// Exit code of a command that failed.
const FAILURE: u8 = 1;

/// Exit code of a command interrupted by Ctrl-C: 128 + SIGINT, as shells report it.
const INTERRUPTED: u8 = 130;

/// Commands still allowed under sudo: they create nothing.
const ALLOWED_UNDER_SUDO: [&str; 1] = ["doctor"];

/// Runs `command` and returns the process exit code. Errors are reported on
/// standard error, never silently.
pub fn run(ctx: &Context, command: &Command) -> u8 {
    match execute(ctx, command) {
        Ok(outcome) => outcome.exit_code(),
        // Whatever failed after a deferred Ctrl-C failed because of it.
        Err(_) if interrupt::received() => {
            ctx.ui().err("\ninterrupted");
            INTERRUPTED
        }
        Err(err) => {
            report_error(ctx.ui(), &err);
            FAILURE
        }
    }
}

/// Runs `command` after the checks common to every command.
pub fn execute(ctx: &Context, command: &Command) -> Result<Outcome> {
    command.validate()?;
    refuse_sudo(ctx.host(), &command.name())?;
    if command.needs_data_volume() {
        ctx.data_volume().ensure_mounted(ctx.ui())?;
    }
    match command {
        Command::Setup(args) => setup::run(ctx, args),
        Command::Doctor(args) => doctor::run(ctx, args),
        Command::Init(args) => init::run(ctx, args),
        Command::New(args) => new::run(ctx, args),
        Command::Ls(args) => ls::run(ctx, args),
        Command::Checkpoint(args) => checkpoint::run(ctx, args),
        Command::Log(args) => log::run(ctx, args),
        Command::Restore(args) => restore::run(ctx, args),
        Command::Rm(args) => rm::run(ctx, args),
        Command::Sync(args) => sync::run(ctx, args),
        Command::Deinit(args) => deinit::run(ctx, args),
        Command::Df(args) => df::run(ctx, args),
        Command::Prune(args) => prune::run(ctx, args),
        Command::Path(args) => path::run(ctx, args),
        Command::Prompt(args) => prompt::run(ctx, args),
        Command::Compose(args) => passthrough::run(ctx, args),
        // Already refused by `validate`.
        Command::Unknown(args) => Err(unknown_command(args)),
    }
}

/// Refuses privilege elevation from a regular account.
///
/// No command needs privileges (`setup` asks sudo itself, for the few steps
/// that do), so `sudo ramet new feat-x` would bring nothing but trouble:
/// subvolumes and `env.json` files would belong to root, and the user could no
/// longer write to them. A genuinely root environment (a container, a machine
/// administered that way) stays consistent with itself and is let through;
/// what is refused is elevation, betrayed by the variable `sudo`, `doas` or
/// `pkexec` leaves behind. (`su -c` leaves none.)
pub fn refuse_sudo(host: &dyn Host, command: &str) -> Result<()> {
    if ALLOWED_UNDER_SUDO.contains(&command) {
        return Ok(());
    }
    match elevated_from(host) {
        Some(user) => Err(Error::SudoRefused {
            command: command.to_owned(),
            user,
        }),
        None => Ok(()),
    }
}

/// The account a root process was elevated from, when it was: `sudo` sets
/// `SUDO_USER`, `doas` sets `DOAS_USER`, and `pkexec` sets `PKEXEC_UID`.
pub fn elevated_from(host: &dyn Host) -> Option<String> {
    if host.effective_uid() != 0 {
        return None;
    }
    ["SUDO_USER", "DOAS_USER", "PKEXEC_UID"]
        .iter()
        .find_map(|name| host.var(name).filter(|value| !value.is_empty()))
}

/// Prints `error: <message>` and, when there is one, the hint that follows.
pub fn report_error(ui: &Ui, err: &Error) {
    let style = ui.style();
    ui.err(format!("{} {err}", style.red("error:")));
    if let Some(hint) = err.hint() {
        let mut lines = hint.lines();
        if let Some(first) = lines.next() {
            ui.err(format!("{} {first}", style.cyan("hint:")));
        }
        for line in lines {
            ui.err(format!("      {line}"));
        }
    }
}
