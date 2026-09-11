//! `ramet prune`: delete, across every project, what no longer belongs to
//! anything.
//!
//! What counts as orphaned is [`inventory`]'s rule: the worktree is gone.
//! Everything is listed with its size first, and deleted only once confirmed.
//! A repository moved elsewhere looks orphaned too, since the path its envs
//! recorded no longer exists: the listing shows that path, for the user to
//! recognize it and answer no.

use std::path::{Path, PathBuf};

use crate::commands::Outcome;
use crate::commands::df::orphan_label;
use crate::commands::support::measured;
use crate::compose::{Stack, StackState};
use crate::context::Context;
use crate::env::inventory::{self, Orphan, Project, Subvolume};
use crate::env::store;
use crate::error::{Error, Result};
use crate::process::RunnerExt;
use crate::storage::Usage;
use crate::util::fs::tilde;
use crate::util::size::human_bytes;

/// Arguments of `ramet prune`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Do not ask for confirmation
    #[arg(short, long)]
    pub yes: bool,
}

/// What to delete in one project.
struct Plan<'a> {
    project: &'a Project,
    /// The orphaned subvolumes.
    subvolumes: Vec<&'a Subvolume>,
    /// What each of them holds.
    own: Vec<Usage>,
    /// Whether the whole project goes, its directory included.
    whole: bool,
    /// Bytes deleting them frees, when measured.
    bytes: Option<u64>,
    /// Whether that figure is a lower bound.
    partial: bool,
}

/// Runs `ramet prune`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let projects = inventory::scan(ctx);
    let plans: Vec<Plan<'_>> = projects
        .iter()
        .filter_map(|project| plan(ctx, project))
        .collect();
    let ui = ctx.ui();
    let root = ctx.layout().root().display();
    if plans.is_empty() {
        ui.out(format!(
            "nothing to prune in {root}: every env and checkpoint has its worktree"
        ));
        return Ok(Outcome::Done);
    }

    ui.out(format!("orphaned in {root}:"));
    for plan in &plans {
        show(ctx, plan);
    }
    let total: u64 = plans.iter().filter_map(|plan| plan.bytes).sum();
    ui.blank();
    ui.out(format!("  at least {} to free", human_bytes(Some(total))));
    if !ui.confirm("  delete them?", args.yes, false)? {
        ui.out("aborted.");
        return Ok(Outcome::Aborted);
    }

    let mut deleted = 0;
    for plan in &plans {
        deleted += execute(ctx, plan)?;
    }
    ui.blank();
    ui.out(ui.style().green(format!(
        "{deleted} subvolume(s) deleted; btrfs releases the space in the background."
    )));
    Ok(Outcome::Done)
}

/// What to delete in `project`, if anything.
fn plan<'a>(ctx: &Context, project: &'a Project) -> Option<Plan<'a>> {
    let subvolumes: Vec<&Subvolume> = project.orphans().collect();
    let whole = project.is_orphaned();
    if subvolumes.is_empty() && !whole {
        return None;
    }
    let sizes = ctx.sizes();
    if subvolumes.is_empty() {
        // An empty project directory: nothing to measure.
        return Some(Plan {
            project,
            subvolumes,
            own: Vec::new(),
            whole,
            bytes: Some(0),
            partial: false,
        });
    }
    let own: Vec<Usage> = subvolumes
        .iter()
        .map(|subvolume| sizes.subvolume(&subvolume.path))
        .collect();
    if whole {
        // Nothing outside a project shares its data: deleting all of it
        // frees what it occupies, shared bytes included.
        let total = sizes.project(&project.dir, &own);
        return Some(Plan {
            project,
            subvolumes,
            own,
            whole,
            bytes: total.bytes,
            partial: total.lower_bound,
        });
    }
    // Each subvolume's own bytes; what the orphans share with each other
    // comes on top, hence "at least".
    Some(Plan {
        project,
        subvolumes,
        whole,
        bytes: Some(own.iter().filter_map(|usage| usage.exclusive_bytes).sum()),
        partial: own.iter().any(|usage| usage.partial),
        own,
    })
}

fn show(ctx: &Context, plan: &Plan<'_>) {
    let ui = ctx.ui();
    let style = ui.style();
    let home = ctx.host().var("HOME").map(PathBuf::from);
    let path = |path: &Path| tilde(path, home.as_deref());
    let size = measured(plan.bytes, plan.partial);
    let project = &plan.project.name;
    ui.blank();
    if plan.subvolumes.is_empty() {
        ui.out(format!(
            "  project {}   {}",
            style.bold(project),
            style.yellow("empty directory")
        ));
    } else if plan.whole {
        let why = if let Some(clone) = plan.project.main_clone() {
            format!(
                "every worktree is gone, the main clone's too ({})",
                path(clone)
            )
        } else if plan
            .subvolumes
            .iter()
            .all(|subvolume| subvolume.orphan == Some(Orphan::NoMetadata))
        {
            "no env.json: the remains of an interrupted `ramet init`".to_owned()
        } else {
            "every worktree is gone".to_owned()
        };
        ui.out(format!(
            "  project {}   {size}   {}",
            style.bold(project),
            style.yellow(why)
        ));
        let names: Vec<&str> = plan.subvolumes.iter().map(|s| s.name.as_str()).collect();
        ui.out(format!("    {}", names.join(", ")));
    } else {
        for (subvolume, usage) in plan.subvolumes.iter().zip(&plan.own) {
            let worktree = subvolume
                .worktree
                .as_deref()
                .map(|worktree| format!(": {}", path(worktree)))
                .unwrap_or_default();
            let reason = subvolume.orphan.map(orphan_label).unwrap_or_default();
            let size = measured(usage.exclusive_bytes, usage.partial);
            ui.out(format!(
                "  {project}/{}   {size}   {}",
                style.bold(&subvolume.name),
                style.yellow(format!("{reason}{worktree}"))
            ));
        }
    }
    for subvolume in &plan.subvolumes {
        if subvolume.checkpoint_of.is_none()
            && store::stack_state(ctx, project, &subvolume.name) != StackState::Down
        {
            ui.out(format!(
                "    {}",
                style.yellow(format!(
                    "the stack of {} runs: it will be stopped",
                    subvolume.name
                ))
            ));
        }
    }
}

/// Deletes what `plan` lists. Returns the number of subvolumes deleted.
fn execute(ctx: &Context, plan: &Plan<'_>) -> Result<usize> {
    let ui = ctx.ui();
    let layout = ctx.layout();
    let project = &plan.project.name;
    for subvolume in &plan.subvolumes {
        let file = layout.compose_file(project, &subvolume.name);
        if subvolume.checkpoint_of.is_none() && file.exists() {
            // The worktree is gone: the subvolume stands in as project
            // directory, the generated configuration holding absolute paths.
            let stack = Stack {
                project_dir: subvolume.path.clone(),
                name: format!("{project}-{}", subvolume.name),
                file,
            };
            // `-v` removes the docker volume objects, which point into the
            // subvolume about to go.
            ctx.runner()
                .run_unchecked(&stack.command(["down", "-v", "--remove-orphans"]));
        }
        ctx.subvolumes().delete(project, &subvolume.path)?;
        ui.ok(format!("{project}/{} deleted", subvolume.name));
    }
    if plan.whole {
        match std::fs::remove_dir(&plan.project.dir) {
            Ok(()) => ui.ok(format!(
                "project directory {} removed",
                plan.project.dir.display()
            )),
            // Something else lies there: say so rather than force.
            Err(source) => ui.warn(
                Error::ProjectDirNotRemovable {
                    path: plan.project.dir.clone(),
                    source,
                }
                .to_string(),
            ),
        }
    } else if let Some(clone) = plan.project.main_clone().filter(|clone| clone.is_dir()) {
        // git still lists the worktrees whose directory disappeared.
        ctx.git().prune_worktrees(clone);
    }
    Ok(plan.subvolumes.len())
}
