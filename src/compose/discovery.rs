//! Which compose files an env uses, and the guard that keeps compose inside
//! the repository.
//!
//! ramet never chooses compose files itself: compose handles `compose.yml`,
//! `compose.override.yml`, `COMPOSE_FILE`, `.env` and interpolation. A project
//! whose files are not at the root declares them in its `.ramet.json`, and
//! ramet hands them to compose through `COMPOSE_FILE`, compose's own mechanism.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::util::fs::{relative_to, resolve};

/// File names docker compose looks for in a directory.
pub const COMPOSE_FILENAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Separator of `COMPOSE_FILE`, pinned through `COMPOSE_PATH_SEPARATOR` so
/// that a value inherited from the shell cannot change how the list is read.
pub const COMPOSE_PATH_SEPARATOR: &str = ":";

/// Converts declared compose files into paths relative to `worktree`.
///
/// Every env has its own worktree: an absolute path would point into the
/// original worktree and not follow the envs created by `ramet new`.
pub fn normalize_compose_files(worktree: &Path, files: &[String]) -> Result<Vec<String>> {
    files
        .iter()
        .map(|file| {
            let full = worktree.join(file);
            let relative: PathBuf =
                relative_to(&full, worktree).ok_or_else(|| Error::ComposeFileOutsideWorktree {
                    file: file.clone(),
                    worktree: worktree.to_owned(),
                })?;
            if !full.is_file() {
                return Err(Error::ComposeFileMissing { path: full });
            }
            Ok(relative.to_string_lossy().into_owned())
        })
        .collect()
}

/// The project directory compose resolves `files` against: the directory of
/// the first file, as `docker compose -f <file>` takes it, or the worktree
/// when compose finds its files by itself.
///
/// Relative paths in the files (`build: ../..`, `env_file: app.env`,
/// `./data:/data`) and the `.env` compose reads all depend on it. Measured on
/// docker compose 5.5.1 with `-f docker/compose/base.yml`: taking the
/// worktree instead fails on `env_file` and reads the wrong `.env`. The name
/// compose would derive from that directory does not matter, since ramet
/// names every stack itself.
pub fn project_directory(worktree: &Path, files: &[String]) -> PathBuf {
    files
        .first()
        .and_then(|first| worktree.join(first).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| worktree.to_owned())
}

/// Refuses a worktree where compose would find no file of its own.
///
/// Without a compose file at the root of the worktree, docker compose climbs
/// the parent directories and takes the first file it finds, outside the
/// repository (verified on docker compose 5.5.1; `--project-directory` does
/// not change that). `ramet init` would then create an env for an unrelated
/// project without a word. A lone `docker-compose.override.yml` does not
/// count: compose ignores it without a base file next to it.
///
/// This checks that compose has something to find; it still does not choose
/// for compose.
pub fn ensure_discoverable(worktree: &Path, shell_compose_file: Option<&str>) -> Result<()> {
    let has_root_file = COMPOSE_FILENAMES
        .iter()
        .any(|name| worktree.join(name).is_file());
    let from_shell = shell_compose_file.is_some_and(|value| !value.is_empty());
    (has_root_file || from_shell || dotenv_declares_compose_file(worktree)).ok_or_else(|| {
        Error::ComposeNotDiscoverable {
            worktree: worktree.to_owned(),
            nested: None,
        }
    })
}

/// The compose file of a subdirectory `dir` of `worktree`, or of a directory
/// between the two, relative to the worktree.
///
/// Run from such a subdirectory, the user most likely meant that file: a
/// project of its own inside a repository, or one part of a larger one.
pub fn nested_compose_file(worktree: &Path, dir: &Path) -> Option<PathBuf> {
    let worktree = resolve(worktree);
    let mut current = resolve(dir);
    while current != worktree && current.starts_with(&worktree) {
        if let Some(name) = COMPOSE_FILENAMES
            .iter()
            .find(|name| current.join(name).is_file())
        {
            return current
                .join(name)
                .strip_prefix(&worktree)
                .ok()
                .map(Path::to_path_buf);
        }
        current.pop();
    }
    None
}

/// Whether the worktree's `.env` sets `COMPOSE_FILE`, which compose reads too.
fn dotenv_declares_compose_file(worktree: &Path) -> bool {
    let Ok(bytes) = fs::read(worktree.join(".env")) else {
        return false;
    };
    String::from_utf8_lossy(&bytes).lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return false;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        line.split('=')
            .next()
            .is_some_and(|key| key.trim() == "COMPOSE_FILE")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    struct Worktree {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn worktree() -> Worktree {
        let dir = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(dir.path()).unwrap();
        Worktree { _dir: dir, path }
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn refuses_a_worktree_without_compose_file() {
        let tree = worktree();
        let err = ensure_discoverable(&tree.path, None).unwrap_err();
        assert_matches!(err, Error::ComposeNotDiscoverable { .. });
        assert!(err.hint().unwrap().contains(".ramet.json"));
    }

    #[test]
    fn finds_the_compose_file_of_a_subdirectory() {
        let tree = worktree();
        write(&tree.path.join("example/compose.yml"), "services: {}\n");
        fs::create_dir_all(tree.path.join("example/deeper")).unwrap();
        for dir in ["example", "example/deeper"] {
            assert_eq!(
                nested_compose_file(&tree.path, &tree.path.join(dir)),
                Some(PathBuf::from("example/compose.yml")),
                "{dir}"
            );
        }
        assert_eq!(nested_compose_file(&tree.path, &tree.path), None);
        assert_eq!(
            nested_compose_file(&tree.path, Path::new("/")),
            None,
            "outside the worktree"
        );
    }

    #[test]
    fn a_lone_override_file_is_not_enough() {
        let tree = worktree();
        write(
            &tree.path.join("docker-compose.override.yml"),
            "services: {}\n",
        );
        assert!(ensure_discoverable(&tree.path, None).is_err());
    }

    #[test]
    fn accepts_the_four_standard_names() {
        for name in COMPOSE_FILENAMES {
            let tree = worktree();
            write(&tree.path.join(name), "services: {}\n");
            assert!(ensure_discoverable(&tree.path, None).is_ok(), "{name}");
        }
    }

    #[test]
    fn accepts_compose_file_declared_in_dotenv() {
        let tree = worktree();
        write(
            &tree.path.join(".env"),
            "# local\nexport COMPOSE_FILE=docker/base.yml\n",
        );
        assert!(ensure_discoverable(&tree.path, None).is_ok());
    }

    #[test]
    fn a_dotenv_without_compose_file_is_not_enough() {
        let tree = worktree();
        write(&tree.path.join(".env"), "DATABASE_URL=postgres://x\n");
        assert!(ensure_discoverable(&tree.path, None).is_err());
    }

    #[test]
    fn accepts_compose_file_from_the_shell() {
        let tree = worktree();
        assert!(ensure_discoverable(&tree.path, Some("x.yml")).is_ok());
        assert!(ensure_discoverable(&tree.path, Some("")).is_err());
    }

    #[test]
    fn normalizes_paths_relative_to_the_worktree() {
        let tree = worktree();
        let base = tree.path.join("docker/compose/base.yml");
        write(&base, "services: {}\n");
        let declared = [
            base.display().to_string(),
            "docker/compose/base.yml".to_owned(),
        ];
        assert_eq!(
            normalize_compose_files(&tree.path, &declared).unwrap(),
            vec!["docker/compose/base.yml", "docker/compose/base.yml"]
        );
    }

    #[test]
    fn refuses_a_file_outside_the_worktree() {
        let tree = worktree();
        let err = normalize_compose_files(&tree.path, &["/etc/hostname".to_owned()]).unwrap_err();
        assert_matches!(err, Error::ComposeFileOutsideWorktree { .. });
        let err = normalize_compose_files(&tree.path, &["../x.yml".to_owned()]).unwrap_err();
        assert_matches!(err, Error::ComposeFileOutsideWorktree { .. });
    }

    #[test]
    fn refuses_a_missing_file() {
        let tree = worktree();
        let err = normalize_compose_files(&tree.path, &["absent.yml".to_owned()]).unwrap_err();
        assert_matches!(err, Error::ComposeFileMissing { .. });
    }
}
