//! `ramet rm`: remove an env, its worktree and its checkpoints.

use crate::commands::Outcome;
use crate::context::Context;
use crate::env::{Env, store};
use crate::error::{Error, Result};
use crate::process::RunnerExt;
use crate::util::fs::resolve;

/// Arguments of `ramet rm`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Name of the env to remove
    pub name: String,

    /// Keep the env's checkpoints
    #[arg(long)]
    pub keep_checkpoints: bool,

    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

/// Runs `ramet rm`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let here = store::current_env(ctx)?;
    let mut envs = store::load_envs(ctx.layout(), &here.project);
    let known: Vec<String> = envs.keys().cloned().collect();
    let Some(target) = envs.remove(&args.name) else {
        return Err(Error::UnknownEnvironment {
            name: args.name.clone(),
            project: here.project,
            known,
        });
    };
    if target.is_primary() {
        return Err(Error::PrimaryEnvironment { name: target.name });
    }
    let main_clone = store::main_clone(ctx, &here);
    if target.name == here.name {
        return Err(Error::CurrentEnvironment {
            name: target.name,
            main_clone,
        });
    }

    let layout = ctx.layout();
    let checkpoints = store::checkpoints_on_disk(ctx, &here.project, &target.name);
    let kept = worktree_kept(ctx, &target, &envs, &main_clone);
    let ui = ctx.ui();
    ui.out(format!("removing env {}", ui.style().bold(&target.name)));
    match &kept {
        Some(reason) => ui.out(format!(
            "  worktree     {} (kept: {reason})",
            target.worktree.display()
        )),
        None => ui.out(format!("  worktree     {}", target.worktree.display())),
    }
    ui.out(format!("  subvolume    {}", target.dir(layout).display()));
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
        let fate = if args.keep_checkpoints {
            "kept"
        } else {
            "deleted"
        };
        ui.out(format!("  checkpoints  {} ({fate})", names.join(", ")));
    }
    if !ui.confirm("  confirm?", args.yes, false)? {
        ui.out("aborted.");
        return Ok(Outcome::Aborted);
    }

    if layout.compose_file(&here.project, &target.name).exists() {
        // `-v` removes the docker volume objects; the data sits in the bind
        // mounts, which docker leaves alone (verified). The subvolume goes
        // right after.
        ctx.runner().run_unchecked(&target.stack(layout).command([
            "down",
            "-v",
            "--remove-orphans",
        ]));
        ui.ok("stack stopped");
    }

    let git = ctx.git();
    if kept.is_none() && target.worktree.exists() {
        git.remove_worktree(&main_clone, &target.worktree)?;
        ui.ok("worktree removed");
    }
    git.prune_worktrees(&main_clone);

    let subvolumes = ctx.subvolumes();
    if !args.keep_checkpoints {
        for checkpoint in &checkpoints {
            subvolumes.delete(&here.project, checkpoint)?;
            ui.ok(format!(
                "checkpoint {} deleted",
                checkpoint.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    subvolumes.delete(&here.project, &target.dir(layout))?;
    ui.ok("subvolume deleted");
    ui.blank();
    ui.out(
        ui.style()
            .green(format!("env \"{}\" removed.", target.name)),
    );
    Ok(Outcome::Done)
}

/// Why the worktree `target` records must survive its removal, if it must.
///
/// `git worktree remove --force` discards uncommitted work, and the path comes
/// from `env.json`: a `ramet new` cut short before it wrote its own, or a file
/// edited by hand, can name the worktree of another env. Only a worktree no
/// other env records, that git lists as a linked worktree of this repository,
/// goes.
fn worktree_kept(
    ctx: &Context,
    target: &Env,
    others: &std::collections::BTreeMap<String, Env>,
    main_clone: &std::path::Path,
) -> Option<String> {
    let worktree = resolve(&target.worktree);
    if !target.worktree.exists() {
        return None;
    }
    if let Some(owner) = others
        .values()
        .find(|other| resolve(&other.worktree) == worktree)
    {
        return Some(format!("it is the worktree of env \"{}\"", owner.name));
    }
    let linked = ctx
        .git()
        .worktrees(main_clone)
        .iter()
        .skip(1)
        .any(|tree| resolve(&tree.path) == worktree);
    (!linked).then(|| "git does not list it as a worktree of this repository".to_owned())
}
