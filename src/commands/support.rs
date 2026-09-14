//! Building blocks shared by several commands.

use crate::compose::{Stack, StackState};
use crate::context::Context;
use crate::env::{Env, store};
use crate::error::{Error, Result};
use crate::interrupt::{self, Deferral};
use crate::process::{Output, RunnerExt};
use crate::util::size::human_bytes;

/// A stack frozen for the duration of a snapshot.
///
/// The source stack is frozen, not stopped: `stop` cut connections for
/// several seconds, and an agent working in the source env would see its
/// tests fail and might start "fixing" sound code. `pause` freezes processes
/// through the cgroup freezer without closing sockets (measured: about 90 ms,
/// TCP connections kept, writes buffered and delivered on thaw). The snapshot
/// is then crash-consistent, which a database's write-ahead log is made for.
///
/// Dropping a `Freeze` that was not [released](Freeze::release) thaws the
/// stack, so that no early return can leave it frozen. Ctrl-C cannot either:
/// it is deferred until the stack is thawed.
pub(crate) struct Freeze<'a> {
    ctx: &'a Context,
    stack: Option<Stack>,
    /// Dropped after the thaw, fields being dropped after `Drop::drop`.
    deferral: Option<Deferral>,
}

impl<'a> Freeze<'a> {
    /// A freeze that freezes nothing, for `--live`.
    pub(crate) fn none(ctx: &'a Context) -> Self {
        Self {
            ctx,
            stack: None,
            deferral: None,
        }
    }

    /// Freezes the stack of `env` if it runs and, in a terminal, the user agrees.
    pub(crate) fn begin(ctx: &'a Context, env: &mut Env) -> Result<Self> {
        env.regenerate(ctx, &[])?;
        let state = store::stack_state(ctx, &env.project, &env.name);
        if state == StackState::Down || !confirm_freeze(ctx, &env.name)? {
            return Ok(Self::none(ctx));
        }
        let stack = env.stack(ctx.layout());
        let pause = stack.command(["pause"]);
        // The stack is thawed from here on, even when `pause` fails or is
        // interrupted halfway: `unpause` on a running stack does no harm.
        let freeze = Self {
            ctx,
            stack: Some(stack),
            deferral: Some(Deferral::begin()),
        };
        ctx.runner().run_checked(&pause)?;
        Ok(freeze)
    }

    /// Thaws the stack now. Returns the outcome of `unpause`, or `None` when
    /// nothing was frozen; [`Error::Interrupted`] when Ctrl-C was pressed
    /// meanwhile, for the command to stop there.
    pub(crate) fn release(mut self) -> Result<Option<Output>> {
        let unpause = self.thaw();
        self.deferral = None;
        if interrupt::received() {
            return Err(Error::Interrupted);
        }
        Ok(unpause)
    }

    fn thaw(&mut self) -> Option<Output> {
        let stack = self.stack.take()?;
        Some(self.ctx.runner().run_unchecked(&stack.command(["unpause"])))
    }
}

impl Drop for Freeze<'_> {
    fn drop(&mut self) {
        self.thaw();
    }
}

/// Asks whether to freeze the stack of `env_name` during a snapshot.
///
/// Freezing is the right default, but the question is still asked in a
/// terminal: someone may be working in that stack. Without a terminal, the
/// stack is frozen.
fn confirm_freeze(ctx: &Context, env_name: &str) -> Result<bool> {
    let ui = ctx.ui();
    if !ui.stdin_is_terminal() {
        return Ok(true);
    }
    ui.out(format!(
        "  the stack of \"{env_name}\" will be frozen during the snapshot \
         (~0.1 s, connections are kept)"
    ));
    ui.out(ui.style().dim(
        "    answering no amounts to `--live`: nothing is frozen, and the snapshot \
         is taken while the database writes",
    ));
    ui.confirm("  freeze?", false, true)
}

/// A size measured by `btrfs filesystem du`, marked `≥` when some
/// directories could not be read and the actual figure is larger.
pub(crate) fn measured(bytes: Option<u64>, partial: bool) -> String {
    let size = human_bytes(bytes);
    if partial { format!("≥ {size}") } else { size }
}

/// The value of an optional option, an empty one counting as absent:
/// `--name "$NAME"` with `NAME` unset means the default, not an empty name.
pub(crate) fn given(value: Option<&String>) -> Option<&str> {
    value.map(String::as_str).filter(|value| !value.is_empty())
}

/// One step undoing part of a multi-step operation.
type UndoStep<'a> = Box<dyn FnOnce(&Context) -> Result<()> + 'a>;

/// Undo steps of a multi-step operation, run in reverse order on failure.
pub(crate) struct Rollback<'a> {
    steps: Vec<UndoStep<'a>>,
}

impl<'a> Rollback<'a> {
    pub(crate) fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Registers the step that undoes what was just done.
    pub(crate) fn push(&mut self, step: impl FnOnce(&Context) -> Result<()> + 'a) {
        self.steps.push(Box::new(step));
    }

    /// Runs every undo step, most recent first. A failing step is reported
    /// and does not prevent the others, nor hide the original error.
    pub(crate) fn unwind(self, ctx: &Context) {
        for step in self.steps.into_iter().rev() {
            if let Err(err) = step(ctx) {
                let ui = ctx.ui();
                ui.err(format!(
                    "{} {err}",
                    ui.style().yellow("incomplete cleanup:")
                ));
            }
        }
    }
}

/// One line saying which env a command acts on: `· project/env (branch)`.
pub(crate) fn context_line(ctx: &Context, env: &Env) -> String {
    let state = if env.worktree.is_dir() {
        ctx.git()
            .current_branch(&env.worktree)
            .unwrap_or_else(|| "detached HEAD".to_owned())
    } else {
        // `ramet doctor` explains what to do about it.
        "worktree missing".to_owned()
    };
    format!("· {}/{} ({state})", env.project, env.name)
}

/// The checkpoint subvolume of `env` labelled `label`, which must exist.
///
/// The label is checked as `checkpoint` checks it when creating one: joined
/// into a path, `c1/../../other/main@c1` would reach a subvolume of another
/// project.
pub(crate) fn existing_checkpoint(
    ctx: &Context,
    env: &Env,
    label: &str,
) -> Result<std::path::PathBuf> {
    crate::env::store::validate_name("label", label)?;
    let path = ctx.layout().checkpoint_dir(&env.project, &env.name, label);
    if ctx.subvolumes().is_subvolume(&path) {
        Ok(path)
    } else {
        Err(Error::CheckpointNotFound {
            label: label.to_owned(),
            env: env.name.clone(),
            path,
        })
    }
}
