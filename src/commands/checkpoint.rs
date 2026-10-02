//! `ramet checkpoint`: a named read-only snapshot of the current env's data.
//!
//! A checkpoint is a return point, not a commit: the data of two branches are
//! never merged. `--delete` removes one.

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

    /// Delete the checkpoint instead of taking it
    #[arg(long, conflicts_with_all = ["live", "message"])]
    pub delete: bool,

    /// Do not ask for confirmation before deleting
    #[arg(short, long, requires = "delete")]
    pub yes: bool,
}

/// Runs `ramet checkpoint`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    if args.delete {
        return delete(ctx, args);
    }
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

/// Deletes the checkpoint `args.label` of the current env.
///
/// Either half is enough to go on: a record whose subvolume is gone is
/// forgotten, and a subvolume `env.json` does not record is deleted.
fn delete(ctx: &Context, args: &Args) -> Result<Outcome> {
    let (mut env, _lock) = store::current_env_locked(ctx)?;
    store::validate_name("label", &args.label)?;
    let layout = ctx.layout();
    let path = layout.checkpoint_dir(&env.project, &env.name, &args.label);
    let subvolumes = ctx.subvolumes();
    let on_disk = subvolumes.is_subvolume(&path);
    let recorded = env.checkpoints.get(&args.label);
    if !on_disk && recorded.is_none() {
        return Err(Error::CheckpointNotFound {
            label: args.label.clone(),
            env: env.name,
            path,
        });
    }

    let ui = ctx.ui();
    let style = ui.style();
    ui.out(format!(
        "deleting checkpoint {} of env {}",
        style.bold(&args.label),
        style.bold(&env.name)
    ));
    if let Some(created_at) = recorded.and_then(|meta| meta.created_at.as_deref()) {
        ui.out(format!("  taken        {created_at}"));
    }
    if let Some(message) = recorded
        .and_then(|meta| meta.message.as_deref())
        .filter(|message| !message.is_empty())
    {
        ui.out(format!("  message      {message}"));
    }
    if on_disk {
        ui.out(format!("  subvolume    {}", path.display()));
    } else {
        ui.out(format!("  subvolume    {} (already gone)", path.display()));
    }
    if !ui.confirm("  confirm?", args.yes, false)? {
        ui.out("aborted.");
        return Ok(Outcome::Aborted);
    }

    // The subvolume first: should its deletion fail, the record still names
    // it and the command can run again.
    if on_disk {
        subvolumes.delete(&env.project, &path)?;
    }
    if env.checkpoints.remove(&args.label).is_some() {
        env.save(layout)?;
    }
    ui.out(format!(
        "{} checkpoint {} of env \"{}\" deleted",
        style.green("✓"),
        style.bold(&args.label),
        env.name
    ));
    Ok(Outcome::Done)
}
