//! `ramet ls`: every env of the project.

use std::path::PathBuf;

use serde::Serialize;

use crate::commands::Outcome;
use crate::compose::StackState;
use crate::context::Context;
use crate::env::{Env, store};
use crate::error::{Error, Result};
use crate::settings::Settings;
use crate::util::fs::to_json_pretty;

/// Arguments of `ramet ls`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// JSON output, for agents: standard output holds nothing else
    #[arg(long)]
    pub json: bool,
}

/// One published port of an env.
#[derive(Clone, Debug, Serialize)]
pub struct Published {
    /// The compose service.
    pub service: String,
    /// Port inside the container.
    pub container_port: u16,
    /// Port on the host.
    pub host_port: u16,
    /// The variables of `.ramet.json` holding it; none where the env keeps
    /// the project's ports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<String>,
}

/// What `ls` reports about one env.
#[derive(Clone, Debug, Serialize)]
pub struct EnvSummary {
    /// Name of the env.
    pub name: String,
    /// Env it was cloned from.
    pub parent: Option<String>,
    /// Branch checked out in its worktree.
    pub branch: Option<String>,
    /// Its worktree.
    pub worktree: PathBuf,
    /// Whether the worktree still exists.
    pub worktree_exists: bool,
    /// Its subvolume.
    pub subvolume: PathBuf,
    /// Whether its stack runs.
    pub state: StackState,
    /// Creation time.
    pub created_at: Option<String>,
    /// Its compose project name.
    pub compose_project: String,
    /// The compose profiles of its `.ramet.json`.
    pub profiles: Vec<String>,
    /// The compose files of its `.ramet.json`.
    pub compose_files: Vec<String>,
    /// Its port block and mapping.
    pub ports: PortsSummary,
    /// Its published ports, by host port.
    pub published: Vec<Published>,
    /// Labels of its checkpoints.
    pub checkpoints: Vec<String>,
}

/// Port block and mapping of an env, without unknown keys.
#[derive(Clone, Debug, Serialize)]
pub struct PortsSummary {
    /// The block, or `None` for an env keeping the project's ports.
    pub range: Option<crate::ports::PortRange>,
    /// Host port by `service:container_port`.
    pub map: crate::ports::PortMap,
}

#[derive(Serialize)]
struct Listing<'a> {
    project: &'a str,
    current: Option<&'a str>,
    envs: &'a [EnvSummary],
}

/// Runs `ramet ls`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let location = store::locate(ctx);
    let Some(project) = location.project else {
        return Err(Error::NoProject {
            worktree: location.worktree,
        });
    };
    let envs = store::load_envs(ctx.layout(), &project);
    let current = location
        .worktree
        .as_deref()
        .and_then(|worktree| store::env_at(&envs, worktree));
    let summaries: Vec<EnvSummary> = envs.values().map(|env| summarize(ctx, env)).collect();

    let ui = ctx.ui();
    if args.json {
        ui.out_raw(&to_json_pretty(&Listing {
            project: &project,
            current,
            envs: &summaries,
        }));
        return Ok(Outcome::Done);
    }

    let style = ui.style();
    ui.out(format!(
        "project {}  ({})",
        style.bold(&project),
        ctx.layout().project_dir(&project).display()
    ));
    for item in &summaries {
        let marker = if Some(item.name.as_str()) == current {
            style.cyan("*")
        } else {
            " ".to_owned()
        };
        let state = match item.state {
            StackState::Up => style.green(item.state),
            StackState::Down => style.dim(item.state),
            StackState::Partial => style.yellow(item.state),
        };
        let parent = item
            .parent
            .as_ref()
            .map(|parent| format!("  ← {parent}"))
            .unwrap_or_default();
        ui.blank();
        ui.out(format!(
            " {marker} {}  [{state}]{parent}",
            style.bold(&item.name)
        ));
        let branch = match (&item.branch, item.worktree_exists) {
            (Some(branch), _) => branch.clone(),
            (None, false) => style.red("worktree missing"),
            (None, true) => "detached HEAD".to_owned(),
        };
        ui.out(format!("     branch   {branch}"));
        ui.out(format!("     worktree {}", item.worktree.display()));
        if !item.profiles.is_empty() {
            ui.out(format!("     profiles {}", item.profiles.join(", ")));
        }
        if !item.compose_files.is_empty() {
            ui.out(format!("     compose  {}", item.compose_files.join(", ")));
        }
        for port in &item.published {
            let variables = if port.variables.is_empty() {
                String::new()
            } else {
                format!("  {}", style.dim(port.variables.join(" ")))
            };
            ui.out(format!(
                "     localhost:{} → {}:{}{variables}",
                port.host_port, port.service, port.container_port
            ));
        }
        if !item.checkpoints.is_empty() {
            ui.out(format!("     checkpoints  {}", item.checkpoints.join(", ")));
        }
    }
    ui.blank();
    // The env is deduced from the working directory: say which one it is, or
    // that there is none, since a command run in the wrong worktree acts
    // without a word.
    let cwd = ctx.cwd().display();
    ui.out(style.dim(if current.is_some() {
        format!("  * env deduced from the working directory ({cwd})")
    } else {
        format!("  no env matches the working directory ({cwd})")
    }));
    Ok(Outcome::Done)
}

/// Everything `ls` says about `env`.
pub fn summarize(ctx: &Context, env: &Env) -> EnvSummary {
    let worktree_exists = env.worktree.is_dir();
    // An unreadable `.ramet.json` is `doctor`'s to report: `ls` lists anyway.
    let settings = env.settings().unwrap_or_default();
    let published = published(env, &settings);
    let compose = settings.compose;
    EnvSummary {
        name: env.name.clone(),
        parent: env.parent.clone(),
        branch: worktree_exists
            .then(|| ctx.git().current_branch(&env.worktree))
            .flatten(),
        worktree: env.worktree.clone(),
        worktree_exists,
        subvolume: env.dir(ctx.layout()),
        state: store::stack_state(ctx, &env.project, &env.name),
        created_at: env.created_at.clone(),
        compose_project: env.compose_project(),
        profiles: compose.profiles,
        compose_files: compose.files,
        ports: PortsSummary {
            range: env.ports.range,
            map: env.ports.map.clone(),
        },
        published,
        checkpoints: env.checkpoints.keys().cloned().collect(),
    }
}

/// Published ports of `env` sorted by host port, with the variables
/// `settings` names them by.
fn published(env: &Env, settings: &Settings) -> Vec<Published> {
    let variables = settings.port_variables();
    let mut published: Vec<Published> = env
        .ports
        .map
        .iter()
        .filter_map(|(key, &host_port)| {
            let (service, container_port) = key.split_once(':')?;
            Some(Published {
                service: service.to_owned(),
                container_port: container_port.parse().ok()?,
                host_port,
                variables: variables
                    .iter()
                    .filter(|(_, named)| named == key && env.named_port(key).is_some())
                    .map(|(variable, _)| variable.clone())
                    .collect(),
            })
        })
        .collect();
    published.sort_by_key(|port| port.host_port);
    published
}
