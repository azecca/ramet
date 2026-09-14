//! Local files synced between envs.
//!
//! A `.env` is ignored by git: `git worktree add` does not copy it, and the
//! application of the new env would not start without it. Copying it verbatim
//! would be worse: its ports would still point at the source env, and the
//! application would silently write into its neighbour's data. Synced files
//! are therefore copied with `<local host>:<port>` rewritten from the source
//! env's ports to the target env's.
//!
//! The files are those git does not track that match the `sync` patterns of
//! the source's `.ramet.json` (every `.env` by default), plus that
//! `.ramet.json` itself when git does not track it: the new env then runs the
//! same way. A tracked file comes with the worktree and belongs to git; ramet
//! never writes one.
//!
//! A symbolic link is copied as a link, pointing where the source's points,
//! and never followed: `node_modules/.bin` holds nothing else.

use std::collections::BTreeSet;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::ports::rewrite_local_ports;
use crate::settings::{FILE_NAME, Settings};
use crate::util::fs::{is_present, is_regular_file, read_regular_file, relative_to};
use crate::util::glob::Pattern;

/// The local files of `worktree` to sync into other envs, relative to it,
/// sorted.
///
/// Directories git ignores as a whole (`node_modules/`, `target/`) are only
/// searched by patterns that name them: see [`Pattern::names`].
pub fn synced_files(ctx: &Context, worktree: &Path) -> Result<Vec<String>> {
    let patterns = Settings::load(worktree)?.sync();
    let globs: Vec<Pattern> = patterns
        .iter()
        .map(|pattern| Pattern::new(pattern))
        .collect();
    let mut pathspecs: Vec<String> = patterns
        .iter()
        .map(|pattern| format!(":(glob){pattern}"))
        .collect();
    pathspecs.push(format!(":(literal){FILE_NAME}"));

    let git = ctx.git();
    let candidates = git.untracked_files(worktree, &pathspecs)?;
    let dirs: BTreeSet<String> = candidates.iter().flat_map(|file| ancestors(file)).collect();
    let ignored = git.ignored(worktree, &dirs.into_iter().collect::<Vec<_>>())?;
    let mut files: Vec<String> = candidates
        .into_iter()
        .filter(|file| {
            if file == FILE_NAME {
                return true;
            }
            // Inside an ignored directory, the outermost one decides.
            let fence = ancestors(file)
                .into_iter()
                .find(|dir| ignored.contains(dir));
            globs.iter().any(|glob| {
                glob.matches(file) && fence.as_deref().is_none_or(|dir| glob.names(dir))
            })
        })
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// The directories above a relative path, outermost first, each with a
/// final `/`: `a/` and `a/b/` for `a/b/c`.
fn ancestors(path: &str) -> Vec<String> {
    path.match_indices('/')
        .map(|(at, _)| path[..=at].to_owned())
        .collect()
}

/// A synced path as it lands in the target env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Synced {
    /// A regular file, with its ports rewritten.
    File(Content),
    /// A symbolic link, and what it points to as it reads.
    Link(PathBuf),
}

impl Synced {
    /// What the path of the source worktree `from` becomes in the target env.
    pub fn read(from: &Source, substitutions: &[(u16, u16)]) -> Result<Self> {
        match from {
            Source::File(path) => Content::read(path, substitutions).map(Self::File),
            Source::Link(target) => Ok(Self::Link(target.clone())),
        }
    }

    /// Whether `to` already holds exactly this.
    pub fn is_at(&self, to: &Path) -> Result<bool> {
        let current = match self {
            Self::File(content) => fs::read(to).map(|bytes| bytes == content.bytes),
            Self::Link(target) => fs::read_link(to).map(|current| current == *target),
        };
        match current {
            Ok(same) => Ok(same),
            // A link where a file is expected, or the other way around.
            Err(err) if err.kind() == io::ErrorKind::InvalidInput => Ok(false),
            Err(source) => Err(Error::io(to, source)),
        }
    }

    /// How the copy is described: `copied`, `copied, 2 port(s) rewritten`,
    /// `copied (link to …)`…
    pub fn describe(&self, verb: &str) -> String {
        match self {
            Self::File(content) => content.describe(verb),
            Self::Link(target) => format!("{verb} (link to {})", target.display()),
        }
    }
}

/// A synced file as it lands in the target env.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Content {
    /// The bytes to write.
    pub bytes: Vec<u8>,
    /// Distinct ports rewritten; `None` for a file that is not valid UTF-8,
    /// copied byte for byte.
    pub rewritten: Option<usize>,
}

impl Content {
    /// Reads the regular file `from`, rewriting its ports along
    /// `substitutions`. A symbolic link is refused: see [`source`].
    pub fn read(from: &Path, substitutions: &[(u16, u16)]) -> Result<Self> {
        let bytes = read_regular_file(from).map_err(|source| Error::io(from, source))?;
        Ok(match String::from_utf8(bytes) {
            Ok(text) => {
                let rewrite = rewrite_local_ports(&text, substitutions);
                Self {
                    bytes: rewrite.text.into_bytes(),
                    rewritten: Some(rewrite.rewritten),
                }
            }
            Err(not_text) => Self {
                bytes: not_text.into_bytes(),
                rewritten: None,
            },
        })
    }

    /// How the copy is described: `copied`, `copied, 2 port(s) rewritten`…
    pub fn describe(&self, verb: &str) -> String {
        match self.rewritten {
            None => format!("{verb} (binary, ports not rewritten)"),
            Some(0) => verb.to_owned(),
            Some(ports) => format!("{verb}, {ports} port(s) rewritten"),
        }
    }
}

/// Where a synced file goes in `worktree`. Refuses a destination whose
/// directory, once symbolic links are followed, lies outside it; the file
/// itself is never written when it is a link (see [`is_symlink`]).
pub fn destination(worktree: &Path, relative: &str) -> Result<PathBuf> {
    let path = worktree.join(relative);
    path.parent()
        .and_then(|dir| relative_to(dir, worktree))
        .is_some()
        .ok_or_else(|| Error::SyncedPathOutsideWorktree {
            path: relative.to_owned(),
        })?;
    Ok(path)
}

/// A synced path of the source worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A regular file.
    File(PathBuf),
    /// A symbolic link, and what it points to as it reads.
    Link(PathBuf),
}

/// The synced path `relative` of the worktree `worktree`, when it is a regular
/// file or a symbolic link inside it, and `None` otherwise.
///
/// A symbolic link is read, never followed: a `.env` pointing at
/// `~/.ssh/id_ed25519`, which a container can create in the worktree it
/// mounts, becomes the same link in the other worktree, and never a copy of
/// the key.
pub fn source(worktree: &Path, relative: &str) -> Result<Option<Source>> {
    let path = destination(worktree, relative)?;
    if is_regular_file(&path) {
        return Ok(Some(Source::File(path)));
    }
    Ok(fs::symlink_metadata(&path)
        .is_ok_and(|meta| meta.is_symlink())
        .then(|| fs::read_link(&path).ok())
        .flatten()
        .map(Source::Link))
}

/// Whether `path` is a symbolic link, which ramet never writes through.
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Copies `files` from the worktree `source` into the brand-new worktree
/// `target`, as they are: their ports are rewritten by [`rewrite_copied`] once
/// the new env has its own. Compose may need them before that, a `.env` that
/// sets `COMPOSE_FILE` or interpolated variables.
///
/// A path already present in `target`, which `git worktree add` put there,
/// is left alone.
pub fn copy_into_new(source: &Path, target: &Path, files: &[String]) -> Result<Copied> {
    let mut copied = Copied::default();
    for relative in files {
        let to = destination(target, relative)?;
        let Some(from) = self::source(source, relative)? else {
            continue;
        };
        if is_present(&to) {
            copied.existing.push(relative.clone());
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::io(parent, source))?;
        }
        match from {
            Source::File(from) => {
                copy_new(&from, &to).map_err(|source| Error::io(&to, source))?;
                copied.files.push(relative.clone());
            }
            Source::Link(points_to) => {
                // Fails on anything already there, a link included.
                std::os::unix::fs::symlink(&points_to, &to)
                    .map_err(|source| Error::io(&to, source))?;
                copied
                    .links
                    .push((relative.clone(), Synced::Link(points_to)));
            }
        }
    }
    Ok(copied)
}

/// What [`copy_into_new`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    /// The regular files copied, their ports still to rewrite.
    pub files: Vec<String>,
    /// The symbolic links copied.
    pub links: Vec<(String, Synced)>,
    /// The paths already present in the target, left alone.
    pub existing: Vec<String>,
}

/// Rewrites the ports of the files [`copy_into_new`] copied into `target`.
/// Returns each file with what was done to it.
pub fn rewrite_copied(
    target: &Path,
    files: &[String],
    substitutions: &[(u16, u16)],
) -> Result<Vec<(String, Synced)>> {
    files
        .iter()
        .map(|relative| {
            let path = target.join(relative);
            let content = Content::read(&path, substitutions)?;
            if content.rewritten.is_some_and(|ports| ports > 0) {
                crate::util::fs::replace_file(&path, &content.bytes, 0o600)?;
            }
            Ok((relative.clone(), Synced::File(content)))
        })
        .collect()
}

/// Copies the regular file `from` to `to`, which must not exist yet, with its
/// permissions and times, as `cp -p` does.
///
/// `to` is created with `O_EXCL`, which never follows a symbolic link: one
/// swapped in after the caller looked makes the copy fail.
fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    let bytes = read_regular_file(from)?;
    let meta = fs::symlink_metadata(from)?;
    let mut file: File = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(to)?;
    file.set_permissions(meta.permissions())?;
    file.write_all(&bytes)?;
    file.set_times(
        FileTimes::new()
            .set_accessed(meta.accessed()?)
            .set_modified(meta.modified()?),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_are_listed_outermost_first() {
        assert_eq!(ancestors("a/b/c"), ["a/", "a/b/"]);
        assert!(ancestors(".env").is_empty());
    }

    #[test]
    fn text_gets_its_ports_rewritten_and_binaries_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join(".env");
        fs::write(&text, "URL=http://localhost:8080/\n").unwrap();
        let content = Content::read(&text, &[(8080, 30_000)]).unwrap();
        assert_eq!(content.bytes, b"URL=http://localhost:30000/\n");
        assert_eq!(content.describe("copied"), "copied, 1 port(s) rewritten");

        let binary = dir.path().join("key.der");
        fs::write(&binary, b"\x00localhost:8080\xff").unwrap();
        let content = Content::read(&binary, &[(8080, 30_000)]).unwrap();
        assert_eq!(content.bytes, b"\x00localhost:8080\xff");
        assert_eq!(
            content.describe("copied"),
            "copied (binary, ports not rewritten)"
        );
    }
}
