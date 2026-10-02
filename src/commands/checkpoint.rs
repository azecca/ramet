//! `ramet checkpoint`: named read-only snapshots of the current env's data,
//! taken with `create`, removed with `delete`, listed with `ls`.
//!
//! A checkpoint is a return point, not a commit: the data of two branches are
//! never merged.

use std::path::PathBuf;

use serde::Serialize;

use crate::commands::Outcome;
use crate::commands::support::{Freeze, measured};
use crate::context::Context;
use crate::env::{Checkpoint, store};
use crate::error::{Error, Result};
use crate::storage::Usage;
use crate::util::fs::{is_present, to_json_pretty};
use crate::util::time::UtcDateTime;

/// Arguments of `ramet checkpoint`.
#[derive(Clone, Debug, clap::Args)]
pub struct Args {
    /// What to do with the env's checkpoints
    #[command(subcommand)]
    pub action: Action,
}

/// An action of `ramet checkpoint`.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Action {
    /// Take a return point of the current env's data
    Create(Create),
    /// Delete a checkpoint of the current env
    Delete(Delete),
    /// List the checkpoints of the current env
    Ls(Ls),
}

/// Arguments of `ramet checkpoint create`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Create {
    /// Label of the checkpoint
    pub label: String,

    /// Do not freeze the stack during the snapshot
    #[arg(long)]
    pub live: bool,

    /// Message stored with the checkpoint
    #[arg(short, long)]
    pub message: Option<String>,
}

/// Arguments of `ramet checkpoint delete`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Delete {
    /// Label of the checkpoint
    pub label: String,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

/// Runs `ramet checkpoint`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    match &args.action {
        Action::Create(args) => create(ctx, args),
        Action::Delete(args) => delete(ctx, args),
        Action::Ls(args) => list(ctx, args),
    }
}

/// Takes the checkpoint `args.label` of the current env.
fn create(ctx: &Context, args: &Create) -> Result<Outcome> {
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
fn delete(ctx: &Context, args: &Delete) -> Result<Outcome> {
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

/// Characters of the commit hash shown in the listing.
const SHORT_HASH: usize = 12;

/// Arguments of `ramet checkpoint ls`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Ls {
    /// JSON output, for agents: standard output holds nothing else
    #[arg(long)]
    pub json: bool,
}

/// What `ls` reports about one checkpoint.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    /// Its label.
    pub label: String,
    /// Creation time.
    pub created_at: Option<String>,
    /// Commit checked out when it was taken.
    pub head: Option<String>,
    /// Its message.
    pub message: Option<String>,
    /// Its subvolume.
    pub path: PathBuf,
    /// Whether the subvolume exists.
    pub exists: bool,
    /// Bytes it references exclusively (`btrfs filesystem du`).
    pub exclusive_bytes: Option<u64>,
    /// Whether that figure is a lower bound.
    pub exclusive_partial: bool,
}

#[derive(Serialize)]
struct History<'a> {
    env: &'a str,
    project: &'a str,
    checkpoints: &'a [Entry],
}

/// Lists the checkpoints of the current env, oldest first.
fn list(ctx: &Context, args: &Ls) -> Result<Outcome> {
    let env = store::current_env(ctx)?;
    let subvolumes = ctx.subvolumes();
    let sizes = ctx.sizes();
    let mut checkpoints: Vec<(&String, &Checkpoint)> = env.checkpoints.iter().collect();
    checkpoints.sort_by(|a, b| a.1.created_at.cmp(&b.1.created_at));
    let entries: Vec<Entry> = checkpoints
        .into_iter()
        .map(|(label, meta)| {
            let path = ctx.layout().checkpoint_dir(&env.project, &env.name, label);
            let exists = subvolumes.is_subvolume(&path);
            let usage = if exists {
                sizes.subvolume(&path)
            } else {
                Usage::default()
            };
            Entry {
                label: label.clone(),
                created_at: meta.created_at.clone(),
                head: meta.head.clone(),
                message: meta.message.clone(),
                path,
                exists,
                exclusive_bytes: usage.exclusive_bytes,
                exclusive_partial: usage.partial,
            }
        })
        .collect();

    let ui = ctx.ui();
    if args.json {
        ui.out_raw(&to_json_pretty(&History {
            env: &env.name,
            project: &env.project,
            checkpoints: &entries,
        }));
        return Ok(Outcome::Done);
    }
    if entries.is_empty() {
        ui.out(format!("no checkpoint for env \"{}\"", env.name));
        return Ok(Outcome::Done);
    }

    let style = ui.style();
    ui.out(format!("checkpoints of {}", style.bold(&env.name)));
    for entry in &entries {
        let head: String = entry
            .head
            .as_deref()
            .unwrap_or("?")
            .chars()
            .take(SHORT_HASH)
            .collect();
        let size = measured(entry.exclusive_bytes, entry.exclusive_partial);
        ui.blank();
        ui.out(format!(
            "  {}   {}",
            style.bold(&entry.label),
            entry.created_at.as_deref().unwrap_or("?")
        ));
        ui.out(format!("     HEAD {head}   used {size}"));
        if let Some(message) = entry.message.as_deref().filter(|m| !m.is_empty()) {
            ui.out(format!("     {message}"));
        }
        if !entry.exists {
            ui.out(format!(
                "     {} ({})",
                style.red("subvolume missing from disk"),
                entry.path.display()
            ));
        }
    }
    if entries.iter().any(|entry| entry.exclusive_partial) {
        ui.blank();
        ui.out(style.dim(
            "  \"≥\": some directories cannot be read without root, so the size is a lower bound.",
        ));
    }
    Ok(Outcome::Done)
}
