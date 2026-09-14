//! `ramet sync`: bring the current env's local files up to date with another
//! env.
//!
//! The files `ramet new` copies (every `.env` by default, see `.ramet.json`)
//! drift apart afterwards: a variable added to the main env's `.env` never
//! reaches the envs cloned before. `sync` copies them again from the parent
//! env, or from any env with `--from`, ports rewritten. A missing file is
//! copied; a file that differs is replaced only after confirmation, since the
//! current env may have changed it on purpose. Nothing is ever deleted, and no
//! file git tracks is written.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;

use crate::commands::Outcome;
use crate::commands::support::given;
use crate::context::Context;
use crate::env::sync::{self, Content};
use crate::env::{Env, store};
use crate::error::{Error, Result};
use crate::ports;
use crate::util::fs::replace_file;

/// Arguments of `ramet sync`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Env to take the files from (default: the env the current one was cloned from)
    #[arg(long, value_name = "ENV")]
    pub from: Option<String>,

    /// Replace the files that differ without asking
    #[arg(short, long)]
    pub yes: bool,
}

/// A file to write into the current env.
struct Write {
    relative: String,
    content: Content,
}

/// Runs `ramet sync`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let target = store::current_env(ctx)?;
    let source = source_env(ctx, &target, given(args.from.as_ref()))?;
    let ui = ctx.ui();
    let style = ui.style();
    ui.out(format!(
        "sync {} from \"{}\"",
        style.bold(&target.name),
        source.name
    ));

    let files = sync::synced_files(ctx, &source.worktree)?;
    let tracked = ctx.git().tracked(&target.worktree, &files)?;
    let substitutions = ports::substitutions(&source.ports.map, &target.ports.map);
    let (mut missing, mut differing, mut unchanged) = (Vec::new(), Vec::new(), 0);
    for relative in files {
        let to = sync::destination(&target.worktree, &relative)?;
        let Some(from) = sync::source(&source.worktree, &relative)? else {
            continue;
        };
        if tracked.contains(&relative) {
            ui.warn(format!(
                "`{relative}` is tracked by git in this worktree: left alone"
            ));
            continue;
        }
        if sync::is_symlink(&to) {
            ui.warn(format!(
                "`{relative}` is a symbolic link in this worktree: left alone"
            ));
            continue;
        }
        let content = Content::read(&from, &substitutions)?;
        match fs::read(&to) {
            Ok(current) if current == content.bytes => unchanged += 1,
            Ok(_) => differing.push(Write { relative, content }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                missing.push(Write { relative, content });
            }
            Err(source) => return Err(Error::io(&to, source)),
        }
    }

    // Asked before anything is written: without a terminal, the refusal
    // leaves the env as it was.
    let replace = !differing.is_empty() && {
        ui.out(format!("  differs from \"{}\":", source.name));
        for write in &differing {
            ui.out(format!("    {}", write.relative));
        }
        ui.confirm(
            &format!(
                "  replace {} file(s) with the version of \"{}\"?",
                differing.len(),
                source.name
            ),
            args.yes,
            false,
        )?
    };

    for write in &missing {
        write_file(&source, &target, write)?;
        ui.ok(format!(
            "`{}` {}",
            write.relative,
            write.content.describe("copied")
        ));
    }
    if replace {
        for write in &differing {
            write_file(&source, &target, write)?;
            ui.ok(format!(
                "`{}` {}",
                write.relative,
                write.content.describe("replaced")
            ));
        }
    } else if !differing.is_empty() {
        ui.note(format!("{} file(s) left as they are", differing.len()));
    }

    let written = missing.len() + if replace { differing.len() } else { 0 };
    if unchanged > 0 {
        ui.note(format!("{unchanged} file(s) already up to date"));
    }
    if written > 0 {
        ui.note(
            "containers read these files when they are created: `ramet compose up -d` applies \
             the changes",
        );
    } else if differing.is_empty() && unchanged == 0 {
        ui.note(format!("nothing to sync from \"{}\"", source.name));
    }
    Ok(if differing.is_empty() || replace || written > 0 {
        Outcome::Done
    } else {
        Outcome::Aborted
    })
}

/// The env `from` names, or else the parent of `target`.
fn source_env(ctx: &Context, target: &Env, from: Option<&str>) -> Result<Env> {
    let mut envs = store::load_envs(ctx.layout(), &target.project);
    let others: Vec<String> = envs
        .keys()
        .filter(|name| **name != target.name)
        .cloned()
        .collect();
    let name = match from {
        Some(name) => name.to_owned(),
        None => target.parent.clone().ok_or_else(|| Error::NoSyncSource {
            env: target.name.clone(),
            known: others.clone(),
        })?,
    };
    if name == target.name {
        return Err(Error::SyncFromItself { env: name });
    }
    let source = envs
        .remove(&name)
        .ok_or_else(|| Error::UnknownEnvironment {
            name,
            project: target.project.clone(),
            known: others,
        })?;
    if !source.worktree.is_dir() {
        return Err(Error::NotAWorktree {
            path: source.worktree,
        });
    }
    Ok(source)
}

/// Writes one file into `target`, creating its directories; a new file takes
/// the permissions of the source's, a replaced one keeps its own.
fn write_file(source: &Env, target: &Env, write: &Write) -> Result<()> {
    let to = sync::destination(&target.worktree, &write.relative)?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|err| Error::io(parent, err))?;
    }
    let from = source.worktree.join(&write.relative);
    let mode = fs::symlink_metadata(&from)
        .map_err(|err| Error::io(&from, err))?
        .permissions()
        .mode();
    replace_file(&to, &write.content.bytes, mode)
}
