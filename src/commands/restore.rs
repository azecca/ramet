//! `ramet restore`: rewind the current env's data to one of its checkpoints.

use crate::commands::Outcome;
use crate::commands::support::existing_checkpoint;
use crate::context::Context;
use crate::env::{Checkpoint, store};
use crate::error::Result;
use crate::process::RunnerExt;
use crate::util::time::UtcDateTime;

/// Label prefix of the safety checkpoint taken before a restore.
const BACKUP_PREFIX: &str = "pre-restore";

/// Arguments of `ramet restore`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Label of the checkpoint to restore
    pub label: String,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,

    /// Take a safety checkpoint before rewinding
    #[arg(long)]
    pub pre_restore: bool,
}

/// Runs `ramet restore`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let mut env = store::current_env(ctx)?;
    let source = existing_checkpoint(ctx, &env, &args.label)?;
    let ui = ctx.ui();
    let style = ui.style();
    let layout = ctx.layout();

    ui.out(format!(
        "restoring {} to checkpoint {}",
        style.bold(&env.name),
        style.bold(&args.label)
    ));
    ui.warn(format!(
        "all data of the env written after \"{}\" will be lost",
        args.label
    ));
    if !ui.confirm("  continue?", args.yes, false)? {
        ui.out("aborted.");
        return Ok(Outcome::Aborted);
    }

    // A safety net is offered in a terminal. Otherwise nothing implicit is
    // created: `--pre-restore` asks for it explicitly.
    let backup = args.pre_restore
        || (!args.yes
            && ui.stdin_is_terminal()
            && ui.confirm("  take a \"pre-restore\" checkpoint first?", false, false)?);
    if backup {
        let label = format!("{BACKUP_PREFIX}-{}", UtcDateTime::now().to_compact());
        let path = layout.checkpoint_dir(&env.project, &env.name, &label);
        ctx.subvolumes()
            .snapshot_read_only(&env.dir(layout), &path)?;
        env.checkpoints.insert(
            label.clone(),
            Checkpoint {
                created_at: Some(UtcDateTime::now().to_iso8601()),
                head: ctx.git().head_sha(&env.worktree),
                message: Some(format!("before restoring to {}", args.label)),
                ..Checkpoint::default()
            },
        );
        ui.ok(format!("checkpoint {label}"));
    }

    // env.json lives in the subvolume: the snapshot would rewind it too,
    // erasing the checkpoints and port mapping acquired since. The metadata
    // is kept in memory and written back afterwards.
    let mut restored = env.clone();

    env.regenerate(ctx, &[])?;
    let stack = env.stack(layout);
    ctx.runner()
        .run_checked(&stack.command(["down", "--remove-orphans"]))?;
    ui.ok("stack stopped");

    let env_dir = env.dir(layout);
    ctx.subvolumes().delete(&env.project, &env_dir)?;
    ctx.subvolumes().snapshot(&source, &env_dir)?;
    ui.ok(format!(
        "subvolume recreated from {}",
        source.file_name().unwrap_or_default().to_string_lossy()
    ));

    restored.save(layout)?;
    restored.regenerate(ctx, &[])?;
    ctx.runner().run_checked(
        &restored
            .stack(layout)
            .command(["up", "-d"])
            .inherit_output(),
    )?;
    ui.blank();
    ui.out(style.green(format!(
        "env \"{}\" restored to \"{}\".",
        env.name, args.label
    )));
    Ok(Outcome::Done)
}
