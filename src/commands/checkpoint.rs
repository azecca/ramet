//! `ramet checkpoint`: a named read-only snapshot of the current env's data.
//!
//! A checkpoint is a return point, not a commit: the data of two branches are
//! never merged.

use crate::commands::Outcome;
use crate::commands::support::Freeze;
use crate::context::Context;
use crate::env::{Checkpoint, store};
use crate::error::{Error, Result};
use crate::util::fs::is_present;
use crate::util::time::UtcDateTime;

/// Arguments of `ramet checkpoint`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Label of the checkpoint
    pub label: String,

    /// Do not freeze the stack during the snapshot
    #[arg(long)]
    pub live: bool,

    /// Message stored with the checkpoint
    #[arg(short, long)]
    pub message: Option<String>,
}

/// Runs `ramet checkpoint`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let (mut env, _lock) = store::current_env_locked(ctx)?;
    store::validate_name("label", &args.label)?;
    let destination = ctx
        .layout()
        .checkpoint_dir(&env.project, &env.name, &args.label);
    if env.checkpoints.contains_key(&args.label) || is_present(&destination) {
        return Err(Error::CheckpointExists {
            label: args.label.clone(),
            env: env.name,
        });
    }

    let freeze = if args.live {
        Freeze::none(ctx)
    } else {
        Freeze::begin(ctx, &mut env)?
    };
    // On failure, dropping the freeze thaws the stack all the same.
    ctx.subvolumes()
        .snapshot_read_only(&env.dir(ctx.layout()), &destination)?;
    freeze.release_or_warn(&env.name)?;

    env.checkpoints.insert(
        args.label.clone(),
        Checkpoint {
            created_at: Some(UtcDateTime::now().to_iso8601()),
            head: ctx.git().head_sha(&env.worktree),
            message: args.message.clone(),
            ..Checkpoint::default()
        },
    );
    env.save(ctx.layout())?;
    let ui = ctx.ui();
    ui.out(format!(
        "{} checkpoint {} of env \"{}\"",
        ui.style().green("✓"),
        ui.style().bold(&args.label),
        env.name
    ));
    ui.out(format!("  {}", destination.display()));
    Ok(Outcome::Done)
}
