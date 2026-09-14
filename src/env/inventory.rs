//! What the data root holds, project by project, and what no longer belongs
//! to anything.
//!
//! One rule decides what is orphaned: the worktree a subvolume belongs to is
//! gone. An env records its worktree in its `env.json`; a checkpoint, being a
//! snapshot of its env, holds a frozen copy of that file, so it names its
//! worktree even once its env was removed (`ramet rm --keep-checkpoints`). A
//! subvolume with no `env.json` at all is the remains of an interrupted
//! `ramet init`.
//!
//! Anything this rule cannot settle is left alone: an unreadable `env.json`,
//! a worktree that exists but that git no longer knows, the checkpoints of an
//! env whose worktree is still there. Those are `ramet doctor`'s to report,
//! and a person's to decide.

use std::path::{Path, PathBuf};

use crate::context::Context;
use crate::env::Env;
use crate::layout::{CHECKPOINT_SEPARATOR, env_file_name};
use crate::util::fs::{is_gone, read_json, sorted_entries};

/// Everything under the data root, one entry per project directory.
pub fn scan(ctx: &Context) -> Vec<Project> {
    let subvolumes = ctx.subvolumes();
    sorted_entries(ctx.layout().root())
        .into_iter()
        .filter(|dir| dir.is_dir())
        .map(|dir| {
            let entries = sorted_entries(&dir);
            let found = entries
                .iter()
                .filter(|path| subvolumes.is_subvolume(path))
                .map(|path| Subvolume::read(path))
                .collect();
            Project {
                name: file_name(&dir),
                dir,
                empty: entries.is_empty(),
                subvolumes: found,
            }
        })
        .collect()
}

/// A project directory under the data root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// Its name, the directory's.
    pub name: String,
    /// The directory.
    pub dir: PathBuf,
    /// Whether the directory holds nothing at all.
    pub empty: bool,
    /// The subvolumes directly in it: envs and checkpoints.
    pub subvolumes: Vec<Subvolume>,
}

impl Project {
    /// Whether nothing in it belongs to anything anymore: every subvolume is
    /// orphaned, or there is none and the directory is empty.
    pub fn is_orphaned(&self) -> bool {
        if self.subvolumes.is_empty() {
            return self.empty;
        }
        self.subvolumes
            .iter()
            .all(|subvolume| subvolume.orphan.is_some())
    }

    /// The worktree of the env of the main clone, as recorded.
    pub fn main_clone(&self) -> Option<&Path> {
        self.subvolumes
            .iter()
            .find(|subvolume| subvolume.primary)
            .and_then(|subvolume| subvolume.worktree.as_deref())
    }

    /// The orphaned subvolumes.
    pub fn orphans(&self) -> impl Iterator<Item = &Subvolume> {
        self.subvolumes
            .iter()
            .filter(|subvolume| subvolume.orphan.is_some())
    }
}

/// An env or checkpoint subvolume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subvolume {
    /// Its name: `feat-a`, or `feat-a@c1` for a checkpoint.
    pub name: String,
    /// Its path.
    pub path: PathBuf,
    /// For a checkpoint, the env it was taken from.
    pub checkpoint_of: Option<String>,
    /// Whether this is the env of a main clone.
    pub primary: bool,
    /// The worktree it belongs to, as its `env.json` records it.
    pub worktree: Option<PathBuf>,
    /// Why it belongs to nothing anymore, if it does not.
    pub orphan: Option<Orphan>,
}

/// Why a subvolume is orphaned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orphan {
    /// The worktree it belongs to no longer exists.
    WorktreeGone,
    /// It has no `env.json`: the remains of an interrupted `ramet init`.
    NoMetadata,
}

impl Subvolume {
    fn read(path: &Path) -> Self {
        let name = file_name(path);
        let checkpoint_of = name
            .split_once(CHECKPOINT_SEPARATOR)
            .map(|(env, _)| env.to_owned());
        let file = path.join(env_file_name());
        let metadata: Option<Env> = read_json(&file);
        // A checkpoint lacking its frozen copy falls back on its env's file.
        let fallback = || {
            let env = checkpoint_of.as_deref()?;
            let sibling = path.with_file_name(env).join(env_file_name());
            Some((read_json::<Env>(&sibling), !is_gone(&sibling)))
        };
        let (metadata, recorded) = match metadata {
            Some(env) => (Some(env), true),
            None if !is_gone(&file) => (None, true),
            None => fallback().unwrap_or((None, false)),
        };
        let worktree = metadata.as_ref().map(|env| env.worktree.clone());
        let orphan = match &worktree {
            // Only a worktree known to be gone: one on an unmounted disk
            // or behind a permission it lacks may still hold work.
            Some(worktree) if is_gone(worktree) => Some(Orphan::WorktreeGone),
            Some(_) => None,
            // An env.json that exists but cannot be read may be precious.
            None if recorded => None,
            None => Some(Orphan::NoMetadata),
        };
        Self {
            primary: checkpoint_of.is_none() && metadata.is_some_and(|env| env.is_primary()),
            name,
            path: path.to_owned(),
            checkpoint_of,
            worktree,
            orphan,
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
