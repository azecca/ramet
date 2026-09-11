//! `ramet compose`: a docker compose command, run on the current env's stack
//! with its project name and generated configuration.

use std::ffi::OsString;

use crate::commands::Outcome;
use crate::commands::support::context_line;
use crate::compose::stack::extract_profiles;
use crate::context::Context;
use crate::env::store;
use crate::error::Result;
use crate::process::RunnerExt;

/// Exit code reported when compose's own exit code does not fit in a byte.
const FALLBACK_EXIT_CODE: u8 = 1;

/// Arguments of `ramet compose`.
#[derive(Clone, Debug, clap::Args)]
pub struct Args {
    /// The docker compose command and its arguments, passed as is
    /// (`ramet compose help` for docker compose's own help)
    #[arg(
        value_name = "COMMAND",
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<OsString>,
}

/// Runs `docker compose <args>` for the current env and forwards its exit code.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let mut env = store::current_env(ctx)?;
    let ui = ctx.ui();
    // The env is deduced from the working directory, and this is the only
    // command that never names its target: name it. On standard error, to
    // keep standard output intact in a pipe, and only in a terminal: a script
    // already knows where it is.
    if ui.stderr_is_terminal() {
        ui.err(ui.style().dim(context_line(ctx, &env)));
    }
    let lossy: Vec<String> = args
        .args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    env.regenerate(ctx, &extract_profiles(&lossy))?;
    let cmd = env.stack(ctx.layout()).command(&args.args).inherit_output();
    let code = ctx.runner().run_unchecked(&cmd).code;
    Ok(Outcome::Exit(
        u8::try_from(code).unwrap_or(FALLBACK_EXIT_CODE),
    ))
}
