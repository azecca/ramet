//! Finding envs on disk and deducing the current one from the working directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::compose::{ComposeConfig, StackState};
use crate::context::Context;
use crate::env::Env;
use crate::error::{Error, Result};
use crate::layout::{Layout, env_file_name, is_checkpoint_name};
use crate::ports::{self, PortRange};
use crate::util::fs::{read_json, resolve, sorted_entries};

/// Where the working directory stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    /// The ramet project of the current repository, if it has one.
    pub project: Option<String>,
    /// The current worktree, if inside a git repository.
    pub worktree: Option<PathBuf>,
}

/// `env.json` files of every env, all projects included, sorted by path.
///
/// A checkpoint is a snapshot of its env's subvolume, so it holds a frozen
/// copy of that env's `env.json` (same name, same worktree, older ports and
/// checkpoints). Checkpoints must be skipped, or that stale copy would pass
/// itself off as the env.
pub fn env_files(layout: &Layout) -> Vec<PathBuf> {
    sorted_entries(layout.root())
        .into_iter()
        .filter(|project| project.is_dir())
        .flat_map(|project| sorted_entries(&project))
        .filter(|dir| is_env_dir(dir))
        .map(|env| env.join(env_file_name()))
        .filter(|file| file.is_file())
        .collect()
}

/// Whether `dir` is the subvolume of an env rather than of a checkpoint.
fn is_env_dir(dir: &Path) -> bool {
    dir.file_name()
        .is_some_and(|name| !is_checkpoint_name(&name.to_string_lossy()))
}

/// Reads the env whose `env.json` is `file`.
///
/// Where the file lies says which env it is: `<root>/<project>/<env>/env.json`.
/// The project and name it records only restate that, and the location wins:
/// a hand-edited or copied file must never point an operation, least of all a
/// deletion, at the directory of another env or another project.
fn read_env(file: &Path) -> Option<Env> {
    let dir = file.parent()?;
    let name = dir.file_name()?.to_str()?;
    let project = dir.parent()?.file_name()?.to_str()?;
    let mut env: Env = read_json(file)?;
    project.clone_into(&mut env.project);
    name.clone_into(&mut env.name);
    Some(env)
}

/// Every env of `project`, by name, checkpoints excluded.
pub fn load_envs(layout: &Layout, project: &str) -> BTreeMap<String, Env> {
    sorted_entries(&layout.project_dir(project))
        .into_iter()
        .filter(|dir| is_env_dir(dir))
        .filter_map(|env| read_env(&env.join(env_file_name())))
        .map(|env| (env.name.clone(), env))
        .collect()
}

/// Port blocks reserved by the envs of every project, except the env `except`
/// of `project`.
///
/// Every project counts: all their stacks publish on the same host, and a
/// stopped stack holds no port the host could report as taken. Two envs of
/// the same name in two projects start from the same block, so only their
/// recorded blocks keep them apart.
pub fn reserved_ranges(layout: &Layout, project: &str, except: Option<&str>) -> Vec<PortRange> {
    env_files(layout)
        .iter()
        .filter_map(|file| read_env(file))
        .filter(|env| env.project != project || Some(env.name.as_str()) != except)
        .filter_map(|env| env.ports.range)
        .collect()
}

/// Allocates a port block for `count` published ports of the env `name` of
/// `project`, avoiding the blocks of every other env of the machine; the block
/// of the env `except` of `project`, which is being replaced, does not count.
pub fn allocate_ports(
    ctx: &Context,
    project: &str,
    name: &str,
    count: usize,
    except: Option<&str>,
) -> Result<PortRange> {
    let reserved = reserved_ranges(ctx.layout(), project, except);
    ports::allocate_range(name, count, &reserved, |port| ctx.host().port_is_free(port))
}

/// The project and worktree the working directory belongs to.
///
/// The project is the one of any env of the current repository, including
/// when the current worktree itself has no env yet.
pub fn locate(ctx: &Context) -> Location {
    let git = ctx.git();
    let Some(worktree) = git.current_worktree(ctx.cwd()) else {
        return Location::default();
    };
    let known: BTreeSet<PathBuf> = git
        .worktrees(&worktree)
        .iter()
        .map(|tree| resolve(&tree.path))
        .collect();
    let project = env_files(ctx.layout())
        .iter()
        .filter_map(|file| read_env(file))
        .find(|env| known.contains(&resolve(&env.worktree)))
        .map(|env| env.project);
    Location {
        project,
        worktree: Some(worktree),
    }
}

/// The env whose worktree contains the working directory.
pub fn current_env(ctx: &Context) -> Result<Env> {
    let worktree = ctx
        .git()
        .current_worktree(ctx.cwd())
        .ok_or_else(|| Error::NotInRepository {
            path: ctx.cwd().to_owned(),
        })?;
    let resolved = resolve(&worktree);
    let found = env_files(ctx.layout())
        .iter()
        .filter_map(|file| read_env(file))
        .find(|env| resolve(&env.worktree) == resolved);
    if let Some(env) = found {
        return Ok(env);
    }
    match locate(ctx).project {
        Some(project) => Err(Error::UnmanagedWorktree { worktree, project }),
        None => Err(Error::NoEnvironment { worktree }),
    }
}

/// The name of the env whose worktree is `worktree`, among `envs`.
pub fn env_at<'e>(envs: &'e BTreeMap<String, Env>, worktree: &Path) -> Option<&'e str> {
    let resolved = resolve(worktree);
    envs.values()
        .find(|env| resolve(&env.worktree) == resolved)
        .map(|env| env.name.as_str())
}

/// Subvolumes directly under the project directory: envs and checkpoints.
pub fn subvolumes(ctx: &Context, project: &str) -> Vec<PathBuf> {
    let subvolumes = ctx.subvolumes();
    sorted_entries(&ctx.layout().project_dir(project))
        .into_iter()
        .filter(|path| subvolumes.is_subvolume(path))
        .collect()
}

/// Checkpoint subvolumes of the env `name`, sorted.
pub fn checkpoints_on_disk(ctx: &Context, project: &str, name: &str) -> Vec<PathBuf> {
    let prefix = crate::layout::checkpoint_name(name, "");
    subvolumes(ctx, project)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|file| file.to_string_lossy().starts_with(&prefix))
        })
        .collect()
}

/// The main clone of the repository `env` belongs to.
pub fn main_clone(ctx: &Context, env: &Env) -> PathBuf {
    ctx.git()
        .worktrees(&env.worktree)
        .into_iter()
        .next()
        .map_or_else(|| env.worktree.clone(), |tree| tree.path)
}

/// Whether the stack of the env `name` runs, judged from its containers and
/// from the services of its cached configuration.
pub fn stack_state(ctx: &Context, project: &str, name: &str) -> StackState {
    let containers = ctx.compose().containers(&format!("{project}-{name}"));
    let declared = std::fs::read_to_string(ctx.layout().compose_file(project, name))
        .ok()
        .and_then(|text| ComposeConfig::from_json(&text).ok())
        .map(|config| config.service_names())
        .unwrap_or_default();
    StackState::from_containers(&containers, &declared)
}

/// Reduces a name to `[a-z0-9-]`: lowercase, every run of other characters
/// replaced by one dash, dashes trimmed at both ends.
pub fn sanitize(name: &str) -> Result<String> {
    let mut clean = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            clean.push(c);
            in_run = false;
        } else if !in_run {
            clean.push('-');
            in_run = true;
        }
    }
    let clean = clean.trim_matches('-');
    if clean.is_empty() {
        return Err(Error::InvalidProjectName {
            name: name.to_owned(),
        });
    }
    Ok(clean.to_owned())
}

/// Checks an env name or checkpoint label: `[A-Za-z0-9][A-Za-z0-9._-]*`.
///
/// This keeps `/` (a path separator) and `@` (the checkpoint separator) out
/// of subvolume names, and forbids a leading `-` that tools would read as an
/// option.
pub fn validate_name(kind: &'static str, name: &str) -> Result<()> {
    crate::util::name::is_plain(name).ok_or_else(|| Error::InvalidName {
        kind,
        name: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn sanitize_lowercases_and_replaces_runs() {
        assert_eq!(sanitize("Mon_Projet").unwrap(), "mon-projet");
        assert_eq!(sanitize("app").unwrap(), "app");
        assert_eq!(sanitize("a...b").unwrap(), "a-b");
        assert_eq!(sanitize("a-_b").unwrap(), "a--b");
    }

    #[test]
    fn sanitize_trims_dashes() {
        assert_eq!(sanitize("__app__").unwrap(), "app");
    }

    #[test]
    fn sanitize_refuses_a_name_that_leaves_nothing() {
        assert_matches!(sanitize("///"), Err(Error::InvalidProjectName { .. }));
    }

    #[test]
    fn validate_name_accepts_ordinary_names() {
        for name in ["feat-a", "v2.3", "a_b", "0"] {
            assert!(validate_name("env name", name).is_ok(), "{name}");
        }
    }

    #[test]
    fn validate_name_refuses_separators_and_leading_dashes() {
        for name in ["feat/a", "feat@a", "-feat", ".hidden", ""] {
            assert!(validate_name("env name", name).is_err(), "{name}");
        }
    }
}
