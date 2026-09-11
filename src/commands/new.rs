//! `ramet new`: clone the current env into a new one.
//!
//! Sequence: freeze the source stack, snapshot its subvolume, thaw, add the
//! git worktree, copy the synced files, allocate ports and rewrite them in
//! those files, write `env.json`, start the new stack. Whatever was created
//! is removed if a step fails; what can be foreseen, such as a compose file
//! that was never committed, is refused before anything is created.

use std::path::{Path, PathBuf};

use crate::commands::Outcome;
use crate::commands::support::{Freeze, Rollback, existing_checkpoint, given};
use crate::compose::discovery::COMPOSE_FILENAMES;
use crate::context::Context;
use crate::env::{Env, Ports, store, sync};
use crate::error::{Error, Result};
use crate::layout::{checkpoint_name, worktrees_root};
use crate::ports::{self, sequential_map};
use crate::process::RunnerExt;
use crate::settings::{FILE_NAME, Settings};
use crate::util::fs::is_present;
use crate::util::time::UtcDateTime;

/// Arguments of `ramet new`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Name of the new env
    pub name: String,

    /// Start from a checkpoint of the current env rather than from its current state
    #[arg(long = "from", value_name = "CHECKPOINT")]
    pub from_checkpoint: Option<String>,

    /// Do not freeze the source stack during the snapshot
    #[arg(long)]
    pub live: bool,

    /// Branch to check out (default: the env name)
    #[arg(long)]
    pub branch: Option<String>,
}

/// Runs `ramet new`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    store::validate_name("env name", &args.name)?;
    let mut source = store::current_env(ctx)?;
    let layout = ctx.layout();
    let target_dir = layout.env_dir(&source.project, &args.name);
    if is_present(&target_dir) {
        return Err(Error::EnvironmentExists {
            name: args.name.clone(),
            path: target_dir,
        });
    }
    let main_clone = ctx
        .git()
        .worktrees(&source.worktree)
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotAWorktree {
            path: source.worktree.clone(),
        })?
        .path;
    let target_worktree = worktrees_root(&main_clone).join(&args.name);
    if is_present(&target_worktree) {
        return Err(Error::PathExists {
            path: target_worktree,
        });
    }
    let (snapshot_source, origin) = match given(args.from_checkpoint.as_ref()) {
        Some(label) => (
            existing_checkpoint(ctx, &source, label)?,
            checkpoint_name(&source.name, label),
        ),
        None => (source.dir(layout), source.name.clone()),
    };
    let branch = given(args.branch.as_ref()).unwrap_or(&args.name).to_owned();
    ensure_worktree_gets_compose_files(ctx, &source, &branch)?;
    let synced = sync::synced_files(ctx, &source.worktree)?;

    let ui = ctx.ui();
    ui.out(format!(
        "new env {} from {origin}",
        ui.style().bold(&args.name)
    ));

    let request = Request {
        args,
        snapshot_source,
        target_dir,
        target_worktree,
        branch,
        main_clone,
        synced,
    };
    let mut rollback = Rollback::new();
    let env = match create(ctx, &mut source, &request, &mut rollback) {
        Ok(env) => env,
        Err(err) => {
            rollback.unwind(ctx);
            // The snapshot carries the source's env.json: never leave a
            // half-created env that `ls` would list.
            let _ = std::fs::remove_file(layout.env_file(&source.project, &args.name));
            return Err(err);
        }
    };

    ui.blank();
    ui.out(ui.style().green(format!("env \"{}\" ready.", env.name)));
    let mut published: Vec<(&String, &u16)> = env.ports.map.iter().collect();
    published.sort_by_key(|&(_, port)| *port);
    for (key, port) in published {
        let (service, container_port) = key.split_once(':').unwrap_or((key, ""));
        ui.out(format!("  localhost:{port} → {service}:{container_port}"));
    }
    ui.out(format!("  cd {}", env.worktree.display()));
    Ok(Outcome::Done)
}

/// Refuses, before anything is created, a worktree that would lack the
/// project's compose files.
///
/// A worktree only holds what is committed. Without its compose file, the new
/// env would fail once its subvolume and worktree exist, and nothing in the
/// error would say that the file was simply never committed.
fn ensure_worktree_gets_compose_files(ctx: &Context, source: &Env, branch: &str) -> Result<()> {
    let git = ctx.git();
    let repository = &source.worktree;
    // An existing branch is checked out as it is; a new one starts from the
    // source's HEAD.
    let existing = git.branch_exists(repository, branch);
    let revision = if existing { branch } else { "HEAD" };
    if !git.has_commit(repository, revision) {
        return Err(Error::NoCommit {
            worktree: repository.clone(),
        });
    }
    let settings = settings_of_new_worktree(ctx, source, revision)?;
    let missing: Vec<String> = if settings.compose.files.is_empty() {
        // Compose finds its file by name: any of the standard ones will do.
        let committed = COMPOSE_FILENAMES
            .iter()
            .any(|name| git.commit_has_file(repository, revision, name));
        let local = COMPOSE_FILENAMES
            .iter()
            .filter(|name| repository.join(name).is_file())
            .map(|&name| name.to_owned());
        if committed {
            Vec::new()
        } else {
            local.collect()
        }
    } else {
        settings
            .compose
            .files
            .into_iter()
            .filter(|file| !git.commit_has_file(repository, revision, file))
            .collect()
    };
    // Nothing to check when compose finds its files some other way (`.env`,
    // the shell's `COMPOSE_FILE`).
    if missing.is_empty() {
        return Ok(());
    }
    Err(Error::ComposeFilesNotCommitted {
        files: missing,
        branch: existing.then(|| branch.to_owned()),
    })
}

/// The `.ramet.json` the new worktree will have: the one of the commit it
/// checks out, or else the source's, which is synced along when git does not
/// track it.
fn settings_of_new_worktree(ctx: &Context, source: &Env, revision: &str) -> Result<Settings> {
    let git = ctx.git();
    if let Some(text) = git.file_at(&source.worktree, revision, FILE_NAME) {
        return Settings::parse(&text, Path::new(&format!("{revision}:{FILE_NAME}")));
    }
    let tracked = git.tracked(&source.worktree, &[FILE_NAME.to_owned()])?;
    if tracked.is_empty() {
        source.settings()
    } else {
        Ok(Settings::default())
    }
}

/// What `new` creates, settled before the first change.
struct Request<'a> {
    args: &'a Args,
    snapshot_source: PathBuf,
    target_dir: PathBuf,
    target_worktree: PathBuf,
    branch: String,
    main_clone: PathBuf,
    /// Local files of the source to sync into the new worktree.
    synced: Vec<String>,
}

fn create(
    ctx: &Context,
    source: &mut Env,
    request: &Request<'_>,
    rollback: &mut Rollback<'_>,
) -> Result<Env> {
    let ui = ctx.ui();
    let layout = ctx.layout();
    let args = request.args;

    // A consistent snapshot: the source stack is frozen for its duration. A
    // checkpoint is already frozen in time.
    let freeze = if args.live || given(args.from_checkpoint.as_ref()).is_some() {
        Freeze::none(ctx)
    } else {
        Freeze::begin(ctx, source)?
    };
    ctx.subvolumes()
        .snapshot(&request.snapshot_source, &request.target_dir)?;
    let (undo_project, undo_snapshot) = (source.project.clone(), request.target_dir.clone());
    rollback.push(move |ctx| ctx.subvolumes().delete(&undo_project, &undo_snapshot));
    ui.ok(format!("subvolume {}", request.target_dir.display()));
    if let Some(thaw) = freeze.release()?
        && !thaw.success()
    {
        ui.err(format!(
            "  {} the stack of \"{}\" was not released: {}",
            ui.style().yellow("!"),
            source.name,
            thaw.last_error_line().unwrap_or("?")
        ));
    }

    let branch = request.branch.clone();
    if let Some(parent) = request.target_worktree.parent()
        && !parent.exists()
    {
        crate::util::fs::create_dir_all(parent)?;
        // The directory of worktrees next to the main clone is ramet's doing:
        // it goes too if it ends up empty. Only then, since `remove_dir`
        // never removes a directory with something in it.
        let created = parent.to_owned();
        rollback.push(move |_| {
            let _ = std::fs::remove_dir(&created);
            Ok(())
        });
    }
    ctx.git()
        .add_worktree(&source.worktree, &request.target_worktree, &branch)?;
    let (main_clone, undo_worktree) = (request.main_clone.clone(), request.target_worktree.clone());
    rollback.push(move |ctx| ctx.git().remove_worktree(&main_clone, &undo_worktree));
    ui.ok(format!(
        "worktree {} (branch {branch})",
        request.target_worktree.display()
    ));

    // Before compose resolves anything: `.ramet.json` says which compose
    // files to use, and a `.env` may set `COMPOSE_FILE` or variables the
    // compose files interpolate.
    let (copied, existing) =
        sync::copy_into_new(&source.worktree, &request.target_worktree, &request.synced)?;
    for relative in &existing {
        ui.warn(format!(
            "`{relative}` already exists in the new worktree: left as it is"
        ));
    }

    let mut env = Env {
        name: args.name.clone(),
        project: source.project.clone(),
        parent: Some(source.name.clone()),
        worktree: request.target_worktree.clone(),
        branch_at_creation: Some(branch),
        created_at: Some(UtcDateTime::now().to_iso8601()),
        ports: Ports::default(),
        checkpoints: std::collections::BTreeMap::new(),
        extra: serde_json::Map::new(),
    };

    // The worktree must exist before the ports: only its compose files tell
    // how many ports are published.
    let config = env.resolve(ctx, &[])?;
    let keys = config.published_ports();
    let range = store::allocate_ports(ctx, &env.project, &env.name, keys.len(), None)?;
    env.ports.range = Some(range);
    env.ports.map = sequential_map(&keys, range);
    env.save(layout)?;
    ui.ok(format!("ports {range}"));

    let substitutions = ports::substitutions(&source.ports.map, &env.ports.map);
    for (relative, content) in sync::rewrite_copied(&env.worktree, &copied, &substitutions)? {
        ui.ok(format!("`{relative}` {}", content.describe("copied")));
    }

    env.regenerate(ctx, &[])?;
    let up = env.stack(layout).command(["up", "-d"]).inherit_output();
    ctx.runner().run_checked(&up)?;
    Ok(env)
}
