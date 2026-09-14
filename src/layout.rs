//! Where ramet keeps its data, and how paths under the data root are named.
//!
//! ```text
//! /srv/ramet/<project>/              project directory (plain directory)
//! ├── main/                          subvolume of the env "main"
//! │   ├── volumes/<volume>/          one directory per named compose volume
//! │   ├── env.json                   env metadata
//! │   └── ramet.compose.json         generated compose configuration (cache)
//! └── main@<label>/                  checkpoint: read-only snapshot of "main"
//! ```
//!
//! The env of the main clone is named after the repository's default branch,
//! `main` here.

use std::path::{Path, PathBuf};

/// Default mount point of the btrfs data volume.
pub const DEFAULT_ROOT: &str = "/srv/ramet";

/// The image file backing the data volume.
///
/// Its directory belongs to root, the file to the user. `mount` resolves the
/// path of a `user,loop` fstab line with root's rights: in a directory the
/// user could write, any program running as them could swap the file for a
/// link to a disk or another user's image and have it mounted (CVE-2026-27456,
/// fixed in util-linux 2.41.4 and 2.42.2). One image per machine, like the
/// mount point.
pub const DATA_IMAGE: &str = "/var/lib/ramet/data.img";

/// Size announced for a freshly created data image, in bytes. The image is
/// sparse: it only occupies what it contains.
pub const DATA_IMAGE_SIZE: u64 = 10 * GIB;

/// The system's table of filesystems, where the data volume is described.
pub const FSTAB: &str = "/etc/fstab";

/// Mount options of the `/etc/fstab` line ramet asks the user to install.
///
/// - `noauto`: nothing is mounted at boot; the first ramet command mounts.
/// - `user`: lets the user mount without privilege. It implies
///   `nosuid,nodev,noexec`, hence `exec` restored right after it.
/// - `user_subvol_rm_allowed`: without it, deleting a checkpoint fails.
/// - `discard=async`: without it, the image file never shrinks when data is
///   deleted (measured: 1 GiB deleted brings the image from 1031 MB to 7 MB
///   in about fifteen seconds).
pub const FSTAB_OPTIONS: &str =
    "noauto,user,exec,loop,noatime,discard=async,user_subvol_rm_allowed";

/// One gibibyte.
pub const GIB: u64 = 1 << 30;

/// Free space below which every command warns that the data volume is filling up.
pub const LOW_SPACE_THRESHOLD: u64 = 4 * GIB;

/// Separator between an env name and a checkpoint label in a subvolume name.
pub const CHECKPOINT_SEPARATOR: char = '@';

const ENV_FILE: &str = "env.json";
const COMPOSE_FILE: &str = "ramet.compose.json";
const VOLUMES_DIR: &str = "volumes";
const WORKTREES_SUFFIX: &str = ".wt";

/// Paths of the data root and of the image that backs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
    data_image: PathBuf,
}

impl Layout {
    /// A layout rooted at `root`, backed by the image file `data_image`.
    pub fn new(root: impl Into<PathBuf>, data_image: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            data_image: data_image.into(),
        }
    }

    /// The standard layout: [`DEFAULT_ROOT`], backed by [`DATA_IMAGE`].
    pub fn standard() -> Self {
        Self::new(DEFAULT_ROOT, DATA_IMAGE)
    }

    /// Mount point of the btrfs data volume.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Image file that backs the data volume by default.
    pub fn data_image(&self) -> &Path {
        &self.data_image
    }

    /// Directory holding every env and checkpoint of `project`.
    pub fn project_dir(&self, project: &str) -> PathBuf {
        self.root.join(project)
    }

    /// Subvolume of the env `env` of `project`.
    pub fn env_dir(&self, project: &str, env: &str) -> PathBuf {
        self.project_dir(project).join(env)
    }

    /// Read-only subvolume of the checkpoint `label` of `env`.
    pub fn checkpoint_dir(&self, project: &str, env: &str, label: &str) -> PathBuf {
        self.env_dir(project, &checkpoint_name(env, label))
    }

    /// Directory holding one sub-directory per named volume of `env`.
    pub fn volumes_dir(&self, project: &str, env: &str) -> PathBuf {
        self.env_dir(project, env).join(VOLUMES_DIR)
    }

    /// Metadata file of `env`.
    pub fn env_file(&self, project: &str, env: &str) -> PathBuf {
        self.env_dir(project, env).join(ENV_FILE)
    }

    /// Generated compose configuration of `env`.
    pub fn compose_file(&self, project: &str, env: &str) -> PathBuf {
        self.env_dir(project, env).join(COMPOSE_FILE)
    }

    /// The `/etc/fstab` line that lets the user mount the data volume.
    ///
    /// Only `ramet setup` adds it, with the user's password, and never edits
    /// an existing line: a malformed fstab can prevent a machine from booting.
    pub fn fstab_line(&self) -> String {
        format!(
            "{} {} btrfs {FSTAB_OPTIONS} 0 0\n",
            fstab_field(&self.data_image),
            fstab_field(&self.root)
        )
    }
}

/// `path` as an fstab field: blanks separate fields, so a blank inside a path
/// is written as an octal escape, as fstab(5) prescribes.
fn fstab_field(path: &Path) -> String {
    let mut field = String::new();
    for c in path.to_string_lossy().chars() {
        match c {
            ' ' => field.push_str("\\040"),
            '\t' => field.push_str("\\011"),
            '\n' => field.push_str("\\012"),
            '\\' => field.push_str("\\134"),
            _ => field.push(c),
        }
    }
    field
}

/// Name of the file every env keeps its metadata in.
pub const fn env_file_name() -> &'static str {
    ENV_FILE
}

/// Subvolume name of the checkpoint `label` of `env`.
pub fn checkpoint_name(env: &str, label: &str) -> String {
    format!("{env}{CHECKPOINT_SEPARATOR}{label}")
}

/// Whether a subvolume name designates a checkpoint rather than an env.
pub fn is_checkpoint_name(name: &str) -> bool {
    name.contains(CHECKPOINT_SEPARATOR)
}

/// Directory where the worktrees of `main_clone` live: next to it, never inside.
pub fn worktrees_root(main_clone: &Path) -> PathBuf {
    let mut name = main_clone.file_name().unwrap_or_default().to_owned();
    name.push(WORKTREES_SUFFIX);
    main_clone.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::standard()
    }

    #[test]
    fn env_paths() {
        let layout = layout();
        assert_eq!(
            layout.env_dir("demo", "feat-a"),
            Path::new("/srv/ramet/demo/feat-a")
        );
        assert_eq!(
            layout.volumes_dir("demo", "feat-a"),
            Path::new("/srv/ramet/demo/feat-a/volumes")
        );
        assert_eq!(
            layout.compose_file("demo", "feat-a"),
            Path::new("/srv/ramet/demo/feat-a/ramet.compose.json")
        );
    }

    #[test]
    fn checkpoint_paths() {
        assert_eq!(
            layout().checkpoint_dir("demo", "feat-a", "c1"),
            Path::new("/srv/ramet/demo/feat-a@c1")
        );
        assert!(is_checkpoint_name("feat-a@c1"));
        assert!(!is_checkpoint_name("feat-a"));
    }

    #[test]
    fn worktrees_live_next_to_the_main_clone() {
        assert_eq!(
            worktrees_root(Path::new("/code/app")),
            Path::new("/code/app.wt")
        );
    }

    #[test]
    fn fstab_line_describes_the_expected_mount() {
        assert_eq!(
            layout().fstab_line(),
            "/var/lib/ramet/data.img /srv/ramet btrfs \
             noauto,user,exec,loop,noatime,discard=async,user_subvol_rm_allowed 0 0\n"
        );
    }

    #[test]
    fn fstab_line_escapes_blanks_in_paths() {
        let layout = Layout::new("/srv/ramet", "/home/Jane Doe/data\\x.img");
        assert!(
            layout
                .fstab_line()
                .starts_with("/home/Jane\\040Doe/data\\134x.img /srv/ramet btrfs "),
            "{}",
            layout.fstab_line()
        );
        assert_eq!(layout.fstab_line().split_whitespace().count(), 6);
    }

    #[test]
    fn fstab_options_keep_the_boot_untouched_and_checkpoints_deletable() {
        let options: Vec<&str> = FSTAB_OPTIONS.split(',').collect();
        for required in [
            "noauto",
            "user",
            "exec",
            "loop",
            "discard=async",
            "user_subvol_rm_allowed",
        ] {
            assert!(options.contains(&required), "missing {required}");
        }
    }
}
