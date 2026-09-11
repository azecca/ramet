//! Filesystem helpers.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Upper bound on symbolic links followed while resolving one path, as the kernel does.
const MAX_SYMLINK_HOPS: usize = 40;

/// Resolves `path` like `realpath -m`: symbolic links are followed for the
/// part of the path that exists, and the rest is appended as written, with
/// `.` and `..` applied.
///
/// Containment checks rely on it: a path is compared to a root only once both
/// have been resolved, so neither `..` nor a symbolic link can smuggle a
/// target out of the root.
pub fn resolve(path: &Path) -> PathBuf {
    let mut hops = 0;
    resolve_inner(path, &mut hops)
}

fn resolve_inner(path: &Path, hops: &mut usize) -> PathBuf {
    let mut resolved = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if *hops >= MAX_SYMLINK_HOPS {
                    continue;
                }
                if let Ok(target) = fs::read_link(&resolved) {
                    *hops += 1;
                    resolved.pop();
                    resolved = resolve_inner(&resolved.join(target), hops);
                }
            }
        }
    }
    resolved
}

/// Whether `path`, once resolved, lies inside `root`, once resolved.
/// Returns the path relative to the resolved root.
pub fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    resolve(path)
        .strip_prefix(resolve(root))
        .ok()
        .map(Path::to_path_buf)
}

/// Finds `program` in `PATH`, like `which`.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a file someone may execute.
pub fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether the current user may write into `path`.
pub fn is_writable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
}

/// Reads and parses a JSON file, or `None` when it is missing or malformed.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Serializes `value` as pretty JSON with sorted keys and a final newline.
///
/// Sorted keys keep generated files stable from one run to the next, which
/// makes them diffable and lets unchanged content be detected byte for byte.
///
/// # Panics
///
/// Panics if `value` cannot be represented as JSON (a map with non-string
/// keys, for instance). No ramet type is like that.
pub fn to_json_pretty<T: Serialize + ?Sized>(value: &T) -> String {
    // Converting to a `Value` first sorts object keys: serde_json maps are ordered.
    let value = serde_json::to_value(value).expect("ramet types always serialize to JSON");
    let mut text = serde_json::to_string_pretty(&value).expect("a JSON value always serializes");
    text.push('\n');
    text
}

/// Writes `contents` to `path` atomically: readers see the old file or the
/// new one, never a truncated one.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, contents)
        .and_then(|()| fs::rename(&temporary, path))
        .map_err(|source| {
            let _ = fs::remove_file(&temporary);
            Error::io(path, source)
        })
}

/// Writes `value` as JSON to `path` atomically.
pub fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &to_json_pretty(value))
}

/// Creates `path` and its missing parents.
pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::io(path, source))
}

/// Lists the entries of a directory, sorted by path; empty when it cannot be read.
pub fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    entries
}

/// Bytes a file occupies on its disk, which for a sparse file is less than
/// its size.
pub fn allocated_bytes(meta: &fs::Metadata) -> u64 {
    // `st_blocks` counts 512-byte units, whatever the filesystem's block size.
    std::os::unix::fs::MetadataExt::blocks(meta) * 512
}

/// `path`, with `home` written `~`, for messages.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// Whether `path` exists, following symbolic links; errors count as absence.
pub fn exists(path: &Path) -> bool {
    path.try_exists().unwrap_or(false)
}

/// Whether `path` exists or is a dangling symbolic link.
pub fn is_present(path: &Path) -> bool {
    exists(path) || fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn tilde_shortens_paths_under_home() {
        let home = Path::new("/home/u");
        assert_eq!(tilde(Path::new("/home/u/dev/app"), Some(home)), "~/dev/app");
        assert_eq!(tilde(home, Some(home)), "~");
        assert_eq!(tilde(Path::new("/home/uv/app"), Some(home)), "/home/uv/app");
        assert_eq!(tilde(Path::new("/srv/ramet"), None), "/srv/ramet");
    }

    #[test]
    fn a_sparse_file_occupies_less_than_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sparse");
        fs::File::create(&file).unwrap().set_len(1 << 30).unwrap();
        let meta = fs::metadata(&file).unwrap();
        assert!(allocated_bytes(&meta) < meta.len());
    }

    #[test]
    fn resolve_applies_parent_components() {
        assert_eq!(resolve(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(
            resolve(Path::new("/srv/ramet/demo/../../etc")),
            Path::new("/srv/etc")
        );
        assert_eq!(resolve(Path::new("/..")), Path::new("/"));
    }

    #[test]
    fn resolve_follows_symbolic_links_of_the_existing_part() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(base.join("real")).unwrap();
        symlink(base.join("real"), base.join("link")).unwrap();
        assert_eq!(
            resolve(&base.join("link/missing/file")),
            base.join("real/missing/file")
        );
    }

    #[test]
    fn resolve_catches_a_link_escaping_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(base.join("tree")).unwrap();
        symlink("/etc", base.join("tree/config")).unwrap();
        assert_eq!(
            relative_to(&base.join("tree/config/passwd"), &base.join("tree")),
            None
        );
    }

    #[test]
    fn resolve_survives_a_symlink_loop() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        symlink(base.join("b"), base.join("a")).unwrap();
        symlink(base.join("a"), base.join("b")).unwrap();
        let _ = resolve(&base.join("a/x"));
    }

    #[test]
    fn relative_to_rejects_paths_outside_the_root() {
        assert_eq!(
            relative_to(Path::new("/w/docker/base.yml"), Path::new("/w")),
            Some(PathBuf::from("docker/base.yml"))
        );
        assert_eq!(
            relative_to(Path::new("/w/../etc/hostname"), Path::new("/w")),
            None
        );
    }

    #[test]
    fn json_is_written_atomically_with_sorted_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        write_json(&path, &serde_json::json!({"b": 1, "a": 2})).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"a\": 2,\n  \"b\": 1\n}\n"
        );
        assert_eq!(
            sorted_entries(dir.path()),
            vec![path.clone()],
            "no temporary file left"
        );
        assert_eq!(
            read_json::<serde_json::Value>(&path),
            Some(serde_json::json!({"a": 2, "b": 1}))
        );
    }

    #[test]
    fn read_json_tolerates_missing_and_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_json::<serde_json::Value>(&dir.path().join("absent.json")),
            None
        );
        let broken = dir.path().join("broken.json");
        fs::write(&broken, "{not json").unwrap();
        assert_eq!(read_json::<serde_json::Value>(&broken), None);
    }

    #[test]
    fn which_finds_a_program_in_path() {
        assert!(which("sh").is_some());
        assert!(which("ramet-program-that-does-not-exist").is_none());
    }
}
