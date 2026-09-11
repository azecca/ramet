//! `ramet deinit`: undo `init` and hand the project back.
//!
//! The counterpart of ramet's reversibility promise. It does not cascade over
//! secondary envs: removing them is a decision to take env by env, each with
//! its own data and worktree. The repository and the original docker volumes
//! are never touched, so `docker compose up -d` starts again as before.

use crate::commands::Outcome;
use crate::context::Context;
use crate::env::store;
use crate::error::{Error, Result};
use crate::process::RunnerExt;

/// Arguments of `ramet deinit`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

/// Runs `ramet deinit`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let env = store::current_env(ctx)?;
    if !env.is_primary() {
        return Err(Error::DeinitOutsideMainClone {
            main_clone: store::main_clone(ctx, &env),
            env: env.name,
        });
    }
    let layout = ctx.layout();
    let remaining: Vec<String> = store::load_envs(layout, &env.project)
        .into_values()
        .filter(|other| !other.is_primary())
        .map(|other| other.name)
        .collect();
    if !remaining.is_empty() {
        return Err(Error::EnvironmentsRemain { names: remaining });
    }

    let project_dir = layout.project_dir(&env.project);
    let checkpoints = store::checkpoints_on_disk(ctx, &env.project, &env.name);
    let ui = ctx.ui();
    ui.out(format!(
        "removing ramet from project {}",
        ui.style().bold(&env.project)
    ));
    ui.out(format!("  data         {}", project_dir.display()));
    ui.out(format!("  subvolume    {}", env.dir(layout).display()));
    if !checkpoints.is_empty() {
        let names: Vec<String> = checkpoints
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        ui.out(format!("  checkpoints  {}", names.join(", ")));
    }
    ui.warn(
        "the data managed by ramet will be lost; the original docker volumes were never touched",
    );
    if !ui.confirm("  confirm?", args.yes, false)? {
        ui.out("aborted.");
        return Ok(Outcome::Aborted);
    }

    if layout.compose_file(&env.project, &env.name).exists() {
        ctx.runner().run_unchecked(
            &env.stack(layout)
                .command(["down", "-v", "--remove-orphans"]),
        );
        ui.ok("stack stopped");
    }
    let subvolumes = ctx.subvolumes();
    for checkpoint in &checkpoints {
        subvolumes.delete(&env.project, checkpoint)?;
        ui.ok(format!(
            "checkpoint {} deleted",
            checkpoint.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    subvolumes.delete(&env.project, &env.dir(layout))?;
    ui.ok("subvolume deleted");

    // Not empty means something unexpected lies there: say so rather than force.
    std::fs::remove_dir(&project_dir).map_err(|source| Error::ProjectDirNotRemovable {
        path: project_dir.clone(),
        source,
    })?;

    ui.blank();
    ui.out(
        ui.style()
            .green(format!("project \"{}\" removed from ramet.", env.project)),
    );
    ui.out(
        "  The repository was never modified: `docker compose up -d` starts again \
         on the original docker volumes.",
    );
    Ok(Outcome::Done)
}
