//! `ramet path`: the worktree of an env, alone on standard output, for
//! `cd "$(ramet path feat-x)"`.

use crate::commands::Outcome;
use crate::context::Context;
use crate::env::store;
use crate::error::{Error, Result};

/// Arguments of `ramet path`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Env to locate (default: the current env)
    pub name: Option<String>,
}

/// Runs `ramet path`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let worktree = match &args.name {
        None => store::current_env(ctx)?.worktree,
        Some(name) => {
            let location = store::locate(ctx);
            let Some(project) = location.project else {
                return Err(Error::NoProject {
                    worktree: location.worktree,
                });
            };
            let mut envs = store::load_envs(ctx.layout(), &project);
            let known: Vec<String> = envs.keys().cloned().collect();
            envs.remove(name)
                .ok_or(Error::UnknownEnvironment {
                    name: name.clone(),
                    project,
                    known,
                })?
                .worktree
        }
    };
    ctx.ui().out(worktree.display());
    Ok(Outcome::Done)
}
