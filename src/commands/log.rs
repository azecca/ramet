//! `ramet log`: checkpoints of the current env.

use std::path::PathBuf;

use serde::Serialize;

use crate::commands::Outcome;
use crate::commands::support::measured;
use crate::context::Context;
use crate::env::store;
use crate::error::Result;
use crate::storage::Usage;
use crate::util::fs::to_json_pretty;

/// Characters of the commit hash shown in the listing.
const SHORT_HASH: usize = 12;

/// Arguments of `ramet log`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// JSON output, for agents: standard output holds nothing else
    #[arg(long)]
    pub json: bool,
}

/// What `log` reports about one checkpoint.
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

/// Runs `ramet log`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let env = store::current_env(ctx)?;
    let subvolumes = ctx.subvolumes();
    let sizes = ctx.sizes();
    let mut checkpoints: Vec<(&String, &crate::env::Checkpoint)> = env.checkpoints.iter().collect();
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
