//! `ramet restore`: rewind the current env's data to one of its checkpoints.

use std::fs;

use crate::commands::Outcome;
use crate::commands::support::existing_checkpoint;
use crate::context::Context;
use crate::env::{Checkpoint, store};
use crate::error::{Error, Result};
use crate::interrupt::{self, Deferral};
use crate::layout::checkpoint_name;
use crate::process::RunnerExt;
use crate::util::fs::is_present;
use crate::util::time::UtcDateTime;

/// Label prefix of the safety checkpoint taken before a restore.
const BACKUP_PREFIX: &str = "pre-restore";

/// Label of the copy of the checkpoint about to replace the env's data. No
/// checkpoint can take it: labels never start with a dot.
pub const INCOMING_LABEL: &str = ".restoring";

/// Label the env's data takes while the copy replaces it, until deleted.
pub const OUTGOING_LABEL: &str = ".replaced";

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
    let (mut env, _lock) = store::current_env_locked(ctx)?;
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

    // The env's data is never deleted before its replacement is in place:
    // the checkpoint is copied beside it first, and the two swap names.
    let subvolumes = ctx.subvolumes();
    let env_dir = env.dir(layout);
    let incoming = layout.env_dir(&env.project, &checkpoint_name(&env.name, INCOMING_LABEL));
    let outgoing = layout.env_dir(&env.project, &checkpoint_name(&env.name, OUTGOING_LABEL));
    // What a restore cut short left: a copy never swapped in, or the data it
    // replaced once the swap was done. The env's data is `env_dir`, whole.
    for leftover in [&incoming, &outgoing] {
        if is_present(leftover) {
            subvolumes.delete(&env.project, leftover)?;
        }
    }
    subvolumes.snapshot(&source, &incoming)?;

    env.regenerate(ctx, &[])?;
    let stack = env.stack(layout);
    let stopped = ctx
        .runner()
        .run_checked(&stack.command(["down", "--remove-orphans"]));
    if let Err(err) = stopped {
        let _ = subvolumes.delete(&env.project, &incoming);
        return Err(err);
    }
    ui.ok("stack stopped");

    swap(&env_dir, &incoming, &outgoing, || restored.save(layout))?;
    ui.ok(format!(
        "data swapped for {}",
        source.file_name().unwrap_or_default().to_string_lossy()
    ));
    subvolumes.delete(&env.project, &outgoing)?;
    if interrupt::received() {
        return Err(Error::Interrupted);
    }

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

/// Puts `incoming` in the place of `current`, which becomes `outgoing`, then
/// runs `record`, Ctrl-C held off for the whole sequence.
///
/// Two renames and the metadata: at no moment is the env's data gone. Should
/// the second rename fail, the first is undone. A process killed in between
/// leaves `outgoing` and no `current`, which `ramet doctor` reports with the
/// command that puts it back.
fn swap(
    current: &std::path::Path,
    incoming: &std::path::Path,
    outgoing: &std::path::Path,
    record: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let _deferral = Deferral::begin();
    fs::rename(current, outgoing).map_err(|source| Error::io(current, source))?;
    if let Err(source) = fs::rename(incoming, current) {
        let _ = fs::rename(outgoing, current);
        return Err(Error::io(incoming, source));
    }
    record()
}
