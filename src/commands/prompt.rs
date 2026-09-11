//! `ramet prompt`: `project:env` for the shell prompt.
//!
//! A prompt runs at every shell prompt: it never mounts the data volume and
//! prints nothing, with success, outside an env. Opening a shell must have no
//! side effect and no error.

use crate::commands::Outcome;
use crate::context::Context;
use crate::env::store;
use crate::error::Result;

/// Arguments of `ramet prompt`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Print the env name only
    #[arg(long)]
    pub short: bool,
}

/// Runs `ramet prompt`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    if !ctx.data_volume().is_usable() {
        return Ok(Outcome::Done);
    }
    if let Ok(env) = store::current_env(ctx) {
        ctx.ui().out(if args.short {
            env.name
        } else {
            format!("{}:{}", env.project, env.name)
        });
    }
    Ok(Outcome::Done)
}
