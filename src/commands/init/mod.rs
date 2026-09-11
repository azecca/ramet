//! `ramet init`: turn the main clone of a compose project into its first env.
//!
//! Creates the project directory and the subvolume of the main env, copies
//! the project's existing docker volumes into it, and starts the stack. Never
//! starts on empty volumes silently: when the data may live under another
//! compose project name, `init` asks in a terminal and refuses elsewhere.

mod migration;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::commands::Outcome;
use crate::commands::support::{Rollback, given};
use crate::compose::ComposeConfig;
use crate::compose::discovery::{nested_compose_file, normalize_compose_files, project_directory};
use crate::context::Context;
use crate::env::{Env, Ports, store};
use crate::error::{Error, Result};
use crate::ports::sequential_map;
use crate::process::RunnerExt;
use crate::settings::Settings;
use crate::util::fs::{create_dir_all, is_present, resolve};
use crate::util::size::human_bytes;
use crate::util::time::UtcDateTime;

use migration::{
    VolumeSizes, choice_required, complete_prefixes, implicit_project_name, migration_plan,
    other_candidates_note,
};

/// Name of the main env when the repository has no recognizable default
/// branch, and `--name` says nothing.
const FALLBACK_ENV_NAME: &str = "main";

/// Arguments of `ramet init`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Allocate a port block to the main env too (by default it keeps the project's ports)
    #[arg(long)]
    pub remap_ports: bool,

    /// Migrate from the docker volumes of another compose project name (asked interactively when needed)
    #[arg(long, value_name = "PREFIX")]
    pub migrate_from: Option<String>,

    /// Start on empty volumes without migrating anything
    #[arg(long)]
    pub no_migrate: bool,

    /// Name of the main env (default: the repository's default branch, such as main or master)
    #[arg(long)]
    pub name: Option<String>,
}

/// A prefix proposed to the user, and why.
struct Proposal {
    prefix: String,
    explanation: Option<String>,
}

/// Everything decided before the first change is made.
struct Plan {
    project: String,
    env_name: String,
    worktree: PathBuf,
    config: ComposeConfig,
    managed: BTreeSet<String>,
    /// Docker volume to copy into each managed volume, when it exists.
    sources: BTreeMap<String, String>,
    /// Compose project whose stack must stop before the copy.
    classic_project: String,
}

/// Runs `ramet init`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let Some(plan) = prepare(ctx, args)? else {
        ctx.ui().out("aborted.");
        return Ok(Outcome::Aborted);
    };
    let mut rollback = Rollback::new();
    let env = match execute(ctx, args, &plan, &mut rollback) {
        Ok(env) => env,
        Err(err) => {
            rollback.unwind(ctx);
            return Err(err);
        }
    };

    let ui = ctx.ui();
    ui.blank();
    ui.out(
        ui.style()
            .green(format!("project \"{}\" initialized.", env.project)),
    );
    ui.out(format!("  data   : {}", env.dir(ctx.layout()).display()));
    ui.out(format!("  stack  : {}", env.compose_project()));
    match env.ports.range {
        None => ui.out("  ports  : unchanged (the usual docker compose setup keeps its URLs)"),
        Some(range) => ui.out(format!("  ports  : block {range}")),
    }
    Ok(Outcome::Done)
}

/// Checks everything and settles the migration source. Returns `None` when
/// the user declines the proposed source.
fn prepare(ctx: &Context, args: &Args) -> Result<Option<Plan>> {
    let (worktree, config, project) = inspect(ctx)?;
    let volumes = config.declared_volumes();
    describe(ctx, &project, &worktree, &config, &volumes);
    let Some(env_name) = main_env_name(ctx, args, &worktree)? else {
        return Ok(None);
    };

    let sizes = VolumeSizes::new(ctx.docker());
    let sources = VolumeSources {
        ctx,
        config: &config,
        compose_project: config.name().unwrap_or(&project),
        managed: &volumes.managed,
    };
    let Some(migration) = settle_migration(args, &sources, &sizes)? else {
        return Ok(None);
    };
    ensure_space(ctx, &migration.sources, &sizes)?;

    Ok(Some(Plan {
        classic_project: migration.classic_project,
        project,
        env_name,
        worktree,
        config,
        managed: volumes.managed,
        sources: migration.sources,
    }))
}

/// Checks that `init` runs once, in the main clone of a compose project, and
/// resolves that project. Returns its worktree, configuration and ramet
/// project name.
fn inspect(ctx: &Context) -> Result<(PathBuf, ComposeConfig, String)> {
    let git = ctx.git();
    let worktree = git
        .current_worktree(ctx.cwd())
        .ok_or_else(|| Error::NotInRepository {
            path: ctx.cwd().to_owned(),
        })?;
    if let Some(main_clone) = git.worktrees(&worktree).first()
        && resolve(&main_clone.path) != resolve(&worktree)
    {
        return Err(Error::InitOutsideMainClone {
            main_clone: main_clone.path.clone(),
            worktree,
        });
    }
    if let Some(project) = store::locate(ctx).project {
        let path = ctx.layout().project_dir(&project);
        return Err(Error::AlreadyInitialized { project, path });
    }

    let settings = Settings::load(&worktree)?;
    let compose_files = normalize_compose_files(&worktree, &settings.compose.files)?;
    let config = ctx
        .compose()
        .resolve(&worktree, &settings.compose.profiles, &compose_files)
        .map_err(|err| match err {
            // Run from a subdirectory holding a compose file, `init` says
            // which file it could have used, and why it does not.
            Error::ComposeNotDiscoverable { worktree, .. } => Error::ComposeNotDiscoverable {
                nested: nested_compose_file(&worktree, ctx.cwd()),
                worktree,
            },
            other => other,
        })?;
    let project = project_name(&worktree, &compose_files, &config)?;
    let project_dir = ctx.layout().project_dir(&project);
    if is_present(&project_dir) {
        return Err(Error::AlreadyInitialized {
            project,
            path: project_dir,
        });
    }
    if !ctx.layout().root().is_dir() {
        return Err(Error::RootMissing {
            root: ctx.layout().root().to_owned(),
        });
    }
    Ok((worktree, config, project))
}

/// The ramet project name: the one the project gives itself, else the name
/// of its repository.
///
/// Compose names a project after its project directory unless told
/// otherwise; with files under `docker/compose/`, that name is `compose`,
/// which says nothing and would be shared by every project laid out that
/// way. Only a name chosen on purpose (a top-level `name:`,
/// `COMPOSE_PROJECT_NAME`) is worth taking.
fn project_name(
    worktree: &Path,
    compose_files: &[String],
    config: &ComposeConfig,
) -> Result<String> {
    let directory_name = worktree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let implicit = implicit_project_name(&project_directory(worktree, compose_files));
    match config.name() {
        Some(name) if name != implicit => store::sanitize(name),
        _ => store::sanitize(&directory_name),
    }
}

/// Names the main env after the repository's default branch. Returns `None`
/// when the user declines the proposed name.
///
/// The env of the main clone outlives whatever branch is checked out there:
/// named after the branch of the moment, it would keep the name of a feature
/// merged long ago. So when the main clone is elsewhere than on the default
/// branch, the name is confirmed in a terminal, and must be given otherwise.
fn main_env_name(ctx: &Context, args: &Args, worktree: &Path) -> Result<Option<String>> {
    if let Some(name) = given(args.name.as_ref()) {
        return store::sanitize(name).map(Some);
    }
    let git = ctx.git();
    let ui = ctx.ui();
    let Some(branch) = git.default_branch(worktree) else {
        ui.note(format!(
            "main env \"{FALLBACK_ENV_NAME}\": no default branch found (--name chooses another name)"
        ));
        return Ok(Some(FALLBACK_ENV_NAME.to_owned()));
    };
    let name = store::sanitize(&branch)?;
    let current = git.current_branch(worktree);
    if current.as_deref() == Some(branch.as_str()) {
        return Ok(Some(name));
    }
    let current = current.map_or_else(|| "a detached HEAD".to_owned(), |b| format!("\"{b}\""));
    if !ui.stdin_is_terminal() {
        return Err(Error::MainEnvNameRequired {
            current,
            branch,
            name,
        });
    }
    ui.out(format!(
        "  the main clone is on {current}, not on the default branch \"{branch}\""
    ));
    if ui.confirm(&format!("  name the main env \"{name}\"?"), false, true)? {
        Ok(Some(name))
    } else {
        ui.note("--name chooses the name of the main env");
        Ok(None)
    }
}

/// Names the docker volumes a project's data may sleep in.
struct VolumeSources<'a> {
    ctx: &'a Context,
    config: &'a ComposeConfig,
    /// The name compose gives the project, which prefixes its volumes.
    compose_project: &'a str,
    managed: &'a BTreeSet<String>,
}

impl VolumeSources<'_> {
    /// The docker volume of each managed volume: `<prefix>_<volume>`, or
    /// without a prefix the name compose resolves for the project.
    fn named(&self, prefix: Option<&str>) -> BTreeMap<String, String> {
        self.managed
            .iter()
            .map(|volume| {
                let source = match prefix {
                    Some(prefix) => format!("{prefix}_{volume}"),
                    None => self.config.volume_name(volume).map_or_else(
                        || format!("{}_{volume}", self.compose_project),
                        str::to_owned,
                    ),
                };
                (volume.clone(), source)
            })
            .collect()
    }

    /// The entries of `sources` whose docker volume does not exist.
    fn missing(&self, sources: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        sources
            .iter()
            .filter(|(_, source)| !self.ctx.docker().volume_exists(source))
            .map(|(volume, source)| (volume.clone(), source.clone()))
            .collect()
    }
}

/// The settled migration source.
struct Migration {
    /// Compose project whose stack must stop before the copy.
    classic_project: String,
    /// Existing docker volume to copy into each managed volume.
    sources: BTreeMap<String, String>,
}

/// Decides which docker volumes to copy. Returns `None` when the user
/// declines the proposed source.
fn settle_migration(
    args: &Args,
    sources: &VolumeSources<'_>,
    sizes: &VolumeSizes<'_>,
) -> Result<Option<Migration>> {
    let mut migrate_from = given(args.migrate_from.as_ref()).map(str::to_owned);
    let mut named = sources.named(migrate_from.as_deref());
    let mut missing = sources.missing(&named);

    if !missing.is_empty() && !args.no_migrate {
        if migrate_from.is_some() {
            return Err(Error::MigrationSourceMissing {
                volumes: missing.into_values().collect(),
            });
        }
        if let Some(proposal) = propose_from_existing_volumes(sources.ctx, &named, &missing, sizes)?
        {
            if !accept(sources.ctx, &proposal, sources.managed, sizes)? {
                return Ok(None);
            }
            migrate_from = Some(proposal.prefix);
            named = sources.named(migrate_from.as_deref());
            missing = sources.missing(&named);
            if !missing.is_empty() {
                return Err(Error::MigrationSourceMissing {
                    volumes: missing.into_values().collect(),
                });
            }
        }
    }

    Ok(Some(Migration {
        classic_project: migrate_from.unwrap_or_else(|| sources.compose_project.to_owned()),
        sources: named
            .into_iter()
            .filter(|(volume, _)| !missing.contains_key(volume))
            .collect(),
    }))
}

/// Asks whether to migrate from the proposed prefix.
///
/// `init` is an interactive command run once: asking beats requiring a flag
/// the user would have to know in advance. Without a terminal, it refuses and
/// gives the options.
fn accept(
    ctx: &Context,
    proposal: &Proposal,
    managed: &BTreeSet<String>,
    sizes: &VolumeSizes<'_>,
) -> Result<bool> {
    let plan = migration_plan(&proposal.prefix, managed, sizes);
    let ui = ctx.ui();
    if !ui.stdin_is_terminal() {
        return Err(Error::MigrationChoiceRequired {
            summary: plan.trim_start().to_owned(),
            options: format!(
                "ramet init --migrate-from {} …\nOr start on empty volumes: ramet init --no-migrate …",
                proposal.prefix
            ),
        });
    }
    ui.out(plan);
    if let Some(explanation) = &proposal.explanation {
        ui.out(explanation);
    }
    ui.confirm(
        &format!("  migrate from \"{}\"?", proposal.prefix),
        false,
        false,
    )
}

/// Prints what `init` found in the project.
fn describe(
    ctx: &Context,
    project: &str,
    worktree: &Path,
    config: &ComposeConfig,
    volumes: &crate::compose::DeclaredVolumes,
) {
    let ui = ctx.ui();
    let style = ui.style();
    ui.out(format!(
        "project {} — worktree {}",
        style.bold(project),
        worktree.display()
    ));
    let fixed = config.fixed_container_names();
    if !fixed.is_empty() {
        let services: Vec<&str> = fixed.keys().map(String::as_str).collect();
        ui.note(format!(
            "`container_name` removed from the generated configuration for {}: \
             otherwise two envs would want the same container",
            services.join(", ")
        ));
        ui.out(format!(
            "    (your containers will be named `{project}-<env>-<service>-1`; \
             the project's compose file is not modified)"
        ));
    }
    let managed: Vec<&str> = volumes.managed.iter().map(String::as_str).collect();
    ui.out(format!(
        "  {} named volume(s) to migrate: {}",
        managed.len(),
        if managed.is_empty() {
            "(none)".to_owned()
        } else {
            managed.join(", ")
        }
    ));
    for volume in &volumes.external {
        ui.warn(format!("volume `{volume}` is external: left as it is"));
    }
}

/// Looks for the missing volumes under other prefixes. Proposes the prefix
/// when exactly one holds them all; refuses with the options otherwise.
fn propose_from_existing_volumes(
    ctx: &Context,
    sources: &BTreeMap<String, String>,
    missing: &BTreeMap<String, String>,
    sizes: &VolumeSizes<'_>,
) -> Result<Option<Proposal>> {
    let known = ctx.docker().volume_names();
    let candidates: BTreeMap<String, Vec<String>> = missing
        .keys()
        .map(|volume| {
            let suffix = format!("_{volume}");
            let names: Vec<String> = known
                .iter()
                .filter(|name| name.ends_with(&suffix) && Some(*name) != sources.get(volume))
                .cloned()
                .collect();
            (volume.clone(), names)
        })
        .filter(|(_, names)| !names.is_empty())
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let managed: BTreeSet<String> = sources.keys().cloned().collect();
    let existing: BTreeSet<&str> = known.iter().map(String::as_str).collect();
    let complete = complete_prefixes(&candidates, &managed, &existing);
    match complete.as_slice() {
        [prefix] => Ok(Some(Proposal {
            prefix: prefix.clone(),
            explanation: other_candidates_note(&candidates, &complete)
                .map(|note| ctx.ui().style().dim(note)),
        })),
        _ => Err(choice_required(&candidates, sources, &complete, sizes)),
    }
}

/// Refuses, before anything is stopped, volumes that would not fit.
///
/// Otherwise the copy fills the volume and fails with an unexplained "No space
/// left on device", after the project's stack was already stopped.
fn ensure_space(
    ctx: &Context,
    present: &BTreeMap<String, String>,
    sizes: &VolumeSizes<'_>,
) -> Result<()> {
    let needed: u64 = present.values().map(|source| sizes.of(source)).sum();
    if needed == 0 {
        return Ok(());
    }
    ctx.ui()
        .note(format!("{} of data to copy", human_bytes(Some(needed))));
    let root = ctx.layout().root();
    let available = ctx
        .host()
        .free_bytes(root)
        .map_err(|source| Error::io(root, source))?;
    (needed <= available).ok_or_else(|| Error::InsufficientSpace {
        needed,
        available,
        root: root.to_owned(),
    })
}

/// Creates the env, copies the volumes and starts the stack, registering an
/// undo step after each change.
fn execute(ctx: &Context, args: &Args, plan: &Plan, rollback: &mut Rollback<'_>) -> Result<Env> {
    let ui = ctx.ui();
    let layout = ctx.layout();

    let project_dir = layout.project_dir(&plan.project);
    create_dir_all(&project_dir)?;
    let undo_dir = project_dir.clone();
    rollback.push(move |_| {
        std::fs::remove_dir(&undo_dir).map_err(|source| Error::io(&undo_dir, source))
    });

    let env_dir = layout.env_dir(&plan.project, &plan.env_name);
    ctx.subvolumes().create(&env_dir)?;
    let (undo_project, undo_env) = (plan.project.clone(), env_dir.clone());
    rollback.push(move |ctx| ctx.subvolumes().delete(&undo_project, &undo_env));
    ui.ok(format!("subvolume {}", env_dir.display()));

    let volumes_dir = layout.volumes_dir(&plan.project, &plan.env_name);
    create_dir_all(&volumes_dir)?;

    // Stop *the* stack whose volumes are copied, designated by its project name.
    ctx.compose().down_project(&plan.classic_project)?;
    ui.ok(format!(
        "stack \"{}\" stopped (docker volumes kept)",
        plan.classic_project
    ));

    for volume in &plan.managed {
        let destination = volumes_dir.join(volume);
        create_dir_all(&destination)?;
        let Some(source) = plan.sources.get(volume) else {
            ui.note(format!(
                "volume `{volume}`: nothing to migrate (never created)"
            ));
            continue;
        };
        ctx.docker().copy_volume(source, &destination)?;
        ui.ok(format!("volume `{volume}` migrated from {source}"));
    }

    let ports = if args.remap_ports {
        let keys = plan.config.published_ports();
        let range = store::allocate_ports(ctx, &plan.project, &plan.env_name, keys.len(), None)?;
        Ports {
            range: Some(range),
            map: sequential_map(&keys, range),
            ..Ports::default()
        }
    } else {
        Ports {
            range: None,
            map: plan.config.original_port_map(),
            ..Ports::default()
        }
    };

    let mut env = Env {
        name: plan.env_name.clone(),
        project: plan.project.clone(),
        parent: None,
        worktree: plan.worktree.clone(),
        branch_at_creation: ctx.git().current_branch(&plan.worktree),
        created_at: Some(UtcDateTime::now().to_iso8601()),
        ports,
        checkpoints: BTreeMap::new(),
        extra: serde_json::Map::new(),
    };
    env.save(layout)?;
    env.regenerate(ctx, &[])?;
    let up = env.stack(layout).command(["up", "-d"]).inherit_output();
    ctx.runner().run_checked(&up)?;
    Ok(env)
}
