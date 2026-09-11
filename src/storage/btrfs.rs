//! btrfs subvolumes: creation, snapshots and guarded deletion.
//!
//! Nothing here needs privileges. With the root subvolume owned by the user
//! and the `user_subvol_rm_allowed` mount option, `subvolume create`,
//! `snapshot`, `snapshot -r`, `property set` and `subvolume delete` all work
//! unprivileged (measured).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::error::{DeletionRefusal, Error, Result};
use crate::process::{Cmd, Runner, RunnerExt};
use crate::util::fs::resolve;

/// Inode number of the root directory of every btrfs subvolume.
const SUBVOLUME_ROOT_INODE: u64 = 256;

/// Space used by a subvolume or a directory tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes the tree holds, shared ones included, when they could be measured.
    pub referenced_bytes: Option<u64>,
    /// Bytes only this tree holds, which deleting it frees, when they could
    /// be measured.
    pub exclusive_bytes: Option<u64>,
    /// Bytes the tree shares with itself or with the rest of the volume
    /// (snapshots, clones), each counted once; only `btrfs filesystem du`
    /// reports it.
    pub shared_bytes: Option<u64>,
    /// Whether the figures are lower bounds: directories unreadable without
    /// root were left out.
    pub partial: bool,
}

impl Usage {
    /// Bytes the tree occupies on the volume, shared ones counted once.
    ///
    /// Deleting a tree nothing outside of it shares with, such as a whole
    /// project, frees exactly that.
    pub fn footprint(&self) -> Option<u64> {
        Some(self.exclusive_bytes? + self.shared_bytes.unwrap_or(0))
    }
}

/// Low-level btrfs operations.
///
/// Callers go through [`Subvolumes`], which adds the safety checks; this
/// trait only exists so that tests can replace the real filesystem.
pub trait Btrfs {
    /// Creates an empty subvolume at `path`. It belongs to whoever creates it.
    fn create_subvolume(&self, path: &Path) -> Result<()>;

    /// Snapshots `source` into `destination`, read-only when `read_only` is set.
    fn snapshot(&self, source: &Path, destination: &Path, read_only: bool) -> Result<()>;

    /// Deletes the subvolume at `path`, without any check.
    fn delete_subvolume(&self, path: &Path) -> Result<()>;

    /// Whether `path` is the root of a subvolume.
    fn is_subvolume(&self, path: &Path) -> bool;

    /// Space used by the tree at `path`, a subvolume or a directory holding
    /// several, as `btrfs filesystem du` measures it: without root, what it
    /// can read.
    fn usage(&self, path: &Path) -> Usage;

    /// The id of the subvolume at `path`, which names its quota group.
    fn subvolume_id(&self, path: &Path) -> Option<u64>;
}

/// btrfs operations through the `btrfs` command line tool.
pub struct BtrfsCli {
    runner: Rc<dyn Runner>,
}

impl BtrfsCli {
    /// Runs `btrfs` through `runner`.
    pub fn new(runner: Rc<dyn Runner>) -> Self {
        Self { runner }
    }

    fn subvolume(&self, args: &[&std::ffi::OsStr]) -> Result<()> {
        let cmd = Cmd::new("btrfs").arg("subvolume").args(args);
        self.runner.run_checked(&cmd).map(drop)
    }
}

impl Btrfs for BtrfsCli {
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        self.subvolume(&["create".as_ref(), path.as_ref()])
    }

    fn snapshot(&self, source: &Path, destination: &Path, read_only: bool) -> Result<()> {
        let mut args = vec!["snapshot".as_ref()];
        if read_only {
            args.push("-r".as_ref());
        }
        args.extend([source.as_os_str(), destination.as_os_str()]);
        self.subvolume(&args)
    }

    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        // A checkpoint is a read-only snapshot, and deleting it unprivileged is
        // refused while the `ro` property is set. Clearing the property needs
        // no privilege, and is a no-op on a writable subvolume.
        let clear_read_only = Cmd::new("btrfs")
            .args(["property", "set"])
            .arg(path)
            .args(["ro", "false"]);
        self.runner.run_unchecked(&clear_read_only);
        self.subvolume(&["delete".as_ref(), path.as_ref()])
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        // Unlike `btrfs subvolume show`, which requires root, this needs nothing.
        fs::metadata(path).is_ok_and(|meta| meta.is_dir() && meta.ino() == SUBVOLUME_ROOT_INODE)
    }

    fn usage(&self, path: &Path) -> Usage {
        let cmd = Cmd::new("btrfs")
            .args(["filesystem", "du", "-s", "--raw"])
            .arg(path);
        let output = self.runner.run_unchecked(&cmd);
        if !output.success() {
            return Usage::default();
        }
        let summary = parse_summary(&output.stdout);
        Usage {
            referenced_bytes: summary.map(|(total, _, _)| total),
            exclusive_bytes: summary.map(|(_, exclusive, _)| exclusive),
            shared_bytes: summary.map(|(_, _, shared)| shared),
            // Without root, directories in mode 0700 (the postgres data
            // directory, for one) cannot be entered: the sum is a lower bound.
            partial: output.stderr.contains("Permission denied"),
        }
    }

    fn subvolume_id(&self, path: &Path) -> Option<u64> {
        // Unlike `btrfs subvolume show`, this needs no privilege (measured).
        let cmd = Cmd::new("btrfs")
            .args(["inspect-internal", "rootid"])
            .arg(path);
        let output = self.runner.run_unchecked(&cmd);
        output
            .success()
            .then(|| output.stdout_trimmed().parse().ok())
            .flatten()
    }
}

/// Extracts the "Total", "Exclusive" and "Set shared" columns from `btrfs
/// filesystem du -s --raw`.
fn parse_summary(stdout: &str) -> Option<(u64, u64, u64)> {
    stdout.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let is_data_row = fields.len() >= 4 && fields[0].bytes().all(|b| b.is_ascii_digit());
        if is_data_row {
            Some((
                fields[0].parse().ok()?,
                fields[1].parse().ok()?,
                fields[2].parse().ok()?,
            ))
        } else {
            None
        }
    })
}

/// Subvolume operations under the data root, with deletion guarded.
pub struct Subvolumes<'a> {
    btrfs: &'a dyn Btrfs,
    root: &'a Path,
}

impl<'a> Subvolumes<'a> {
    /// Operations on subvolumes under `root`.
    pub fn new(btrfs: &'a dyn Btrfs, root: &'a Path) -> Self {
        Self { btrfs, root }
    }

    /// Creates an empty subvolume.
    pub fn create(&self, path: &Path) -> Result<()> {
        self.btrfs.create_subvolume(path)
    }

    /// Creates a writable snapshot of `source` at `destination`.
    pub fn snapshot(&self, source: &Path, destination: &Path) -> Result<()> {
        self.btrfs.snapshot(source, destination, false)
    }

    /// Creates a read-only snapshot of `source` at `destination`.
    pub fn snapshot_read_only(&self, source: &Path, destination: &Path) -> Result<()> {
        self.btrfs.snapshot(source, destination, true)
    }

    /// Whether `path` is the root of a subvolume.
    pub fn is_subvolume(&self, path: &Path) -> bool {
        self.btrfs.is_subvolume(path)
    }

    /// Deletes a subvolume of `project` after checking that it is one of ramet's.
    pub fn delete(&self, project: &str, path: &Path) -> Result<()> {
        let target = self.check_deletable(project, path)?;
        self.btrfs.delete_subvolume(&target)
    }

    /// Checks twice that `path` may be deleted by a command acting in
    /// `project`, and returns it resolved.
    ///
    /// 1. Once resolved, it is exactly `<root>/<project>/<env>`: one level
    ///    higher would take every env of a project with it, and the envs of
    ///    another project are never a target.
    /// 2. It is a btrfs subvolume, not an ordinary directory.
    pub fn check_deletable(&self, project: &str, path: &Path) -> Result<PathBuf> {
        let target = resolve(path);
        let root = resolve(self.root);
        let refuse = |reason| Error::DeletionRefused {
            path: target.clone(),
            reason,
        };
        let relative = target
            .strip_prefix(&root)
            .map_err(|_| refuse(DeletionRefusal::OutsideRoot { root: root.clone() }))?;
        let depth = relative.components().count();
        if depth != 2 {
            return Err(refuse(DeletionRefusal::WrongDepth { depth }));
        }
        let project_dir = root.join(project);
        if target.parent() != Some(project_dir.as_path()) {
            return Err(refuse(DeletionRefusal::OtherProject { project_dir }));
        }
        if !self.btrfs.is_subvolume(&target) {
            return Err(refuse(DeletionRefusal::NotSubvolume));
        }
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::cell::RefCell;
    use std::io;

    use crate::process::Output;

    /// Records commands, answers success, and treats every directory as a subvolume.
    #[derive(Default)]
    struct Recorder {
        commands: RefCell<Vec<Vec<String>>>,
    }

    impl Runner for Recorder {
        fn run(&self, cmd: &Cmd) -> io::Result<Output> {
            self.commands.borrow_mut().push(cmd.argv());
            Ok(Output::default())
        }
    }

    struct DirectoriesAreSubvolumes(BtrfsCli);

    impl Btrfs for DirectoriesAreSubvolumes {
        fn create_subvolume(&self, path: &Path) -> Result<()> {
            self.0.create_subvolume(path)
        }
        fn snapshot(&self, source: &Path, destination: &Path, read_only: bool) -> Result<()> {
            self.0.snapshot(source, destination, read_only)
        }
        fn delete_subvolume(&self, path: &Path) -> Result<()> {
            self.0.delete_subvolume(path)
        }
        fn is_subvolume(&self, path: &Path) -> bool {
            path.is_dir()
        }
        fn usage(&self, path: &Path) -> Usage {
            self.0.usage(path)
        }
        fn subvolume_id(&self, path: &Path) -> Option<u64> {
            self.0.subvolume_id(path)
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        runner: Rc<Recorder>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(dir.path()).unwrap().join("srv");
            fs::create_dir(&root).unwrap();
            Self {
                _dir: dir,
                root,
                runner: Rc::new(Recorder::default()),
            }
        }

        fn btrfs(&self) -> DirectoriesAreSubvolumes {
            DirectoriesAreSubvolumes(BtrfsCli::new(self.runner.clone()))
        }

        fn commands(&self) -> Vec<Vec<String>> {
            self.runner.commands.borrow().clone()
        }

        fn refusal(&self, path: &Path) -> DeletionRefusal {
            let btrfs = self.btrfs();
            match Subvolumes::new(&btrfs, &self.root).delete("demo", path) {
                Err(Error::DeletionRefused { reason, .. }) => reason,
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn refuses_a_path_outside_the_root() {
        let fixture = Fixture::new();
        let outside = fixture.root.parent().unwrap().join("elsewhere");
        assert_matches!(
            fixture.refusal(&outside),
            DeletionRefusal::OutsideRoot { .. }
        );
        assert!(
            fixture.commands().is_empty(),
            "nothing may run after a refusal"
        );
    }

    #[test]
    fn refuses_a_project_directory() {
        // One level too high: every env of the project would go.
        let fixture = Fixture::new();
        let project = fixture.root.join("demo");
        fs::create_dir(&project).unwrap();
        assert_eq!(
            fixture.refusal(&project),
            DeletionRefusal::WrongDepth { depth: 1 }
        );
    }

    #[test]
    fn refuses_a_path_below_an_env() {
        let fixture = Fixture::new();
        let volumes = fixture.root.join("demo/main/volumes");
        fs::create_dir_all(&volumes).unwrap();
        assert_eq!(
            fixture.refusal(&volumes),
            DeletionRefusal::WrongDepth { depth: 3 }
        );
    }

    #[test]
    fn refuses_a_path_climbing_out_of_the_root() {
        let fixture = Fixture::new();
        let climbing = fixture.root.join("demo/../../etc");
        assert_matches!(
            fixture.refusal(&climbing),
            DeletionRefusal::OutsideRoot { .. }
        );
    }

    #[test]
    fn refuses_a_symlink_pointing_outside_the_root() {
        let fixture = Fixture::new();
        let outside = fixture.root.parent().unwrap().join("victim");
        fs::create_dir_all(outside.join("env")).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.root.join("demo")).unwrap();
        assert_matches!(
            fixture.refusal(&fixture.root.join("demo/env")),
            DeletionRefusal::OutsideRoot { .. }
        );
    }

    #[test]
    fn refuses_an_env_of_another_project() {
        let fixture = Fixture::new();
        let foreign = fixture.root.join("other/feat-a");
        fs::create_dir_all(&foreign).unwrap();
        assert_eq!(
            fixture.refusal(&foreign),
            DeletionRefusal::OtherProject {
                project_dir: fixture.root.join("demo")
            }
        );
    }

    #[test]
    fn refuses_a_path_reaching_another_project_through_the_project() {
        // `demo/../other/feat-a`, or `demo` itself a link to `other`: once
        // resolved, both lead out of the project.
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.root.join("other/feat-a")).unwrap();
        assert_matches!(
            fixture.refusal(&fixture.root.join("demo/../other/feat-a")),
            DeletionRefusal::OtherProject { .. }
        );
        std::os::unix::fs::symlink(fixture.root.join("other"), fixture.root.join("demo")).unwrap();
        assert_matches!(
            fixture.refusal(&fixture.root.join("demo/feat-a")),
            DeletionRefusal::OtherProject { .. }
        );
        assert!(fixture.commands().is_empty());
    }

    #[test]
    fn refuses_an_ordinary_directory() {
        // Right depth, but not a subvolume: the second check.
        let fixture = Fixture::new();
        let env = fixture.root.join("demo/main");
        fs::create_dir_all(&env).unwrap();
        let real = BtrfsCli::new(fixture.runner.clone());
        let err = Subvolumes::new(&real, &fixture.root)
            .delete("demo", &env)
            .unwrap_err();
        assert_matches!(
            err,
            Error::DeletionRefused {
                reason: DeletionRefusal::NotSubvolume,
                ..
            }
        );
    }

    #[test]
    fn deletion_clears_the_read_only_property_first() {
        let fixture = Fixture::new();
        let checkpoint = fixture.root.join("demo/main@c1");
        fs::create_dir_all(&checkpoint).unwrap();
        let btrfs = fixture.btrfs();
        Subvolumes::new(&btrfs, &fixture.root)
            .delete("demo", &checkpoint)
            .unwrap();
        let path = checkpoint.display().to_string();
        assert_eq!(
            fixture.commands(),
            vec![
                vec!["btrfs", "property", "set", &path, "ro", "false"],
                vec!["btrfs", "subvolume", "delete", &path],
            ]
        );
    }

    #[test]
    fn no_operation_uses_sudo() {
        let fixture = Fixture::new();
        let btrfs = BtrfsCli::new(fixture.runner.clone());
        let root = &fixture.root;
        btrfs.create_subvolume(&root.join("demo/main")).unwrap();
        btrfs
            .snapshot(&root.join("a"), &root.join("b"), false)
            .unwrap();
        btrfs
            .snapshot(&root.join("a"), &root.join("c"), true)
            .unwrap();
        btrfs.delete_subvolume(&root.join("demo/main")).unwrap();
        let commands = fixture.commands();
        assert!(commands.iter().all(|cmd| cmd[0] == "btrfs"), "{commands:?}");
        assert_eq!(
            commands[0].len(),
            4,
            "creation is a single command, no chown"
        );
        assert!(commands[2].contains(&"-r".to_owned()), "read-only snapshot");
    }

    #[test]
    fn an_ordinary_directory_is_not_a_subvolume() {
        let dir = tempfile::tempdir().unwrap();
        let btrfs = BtrfsCli::new(Rc::new(Recorder::default()));
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        assert!(!btrfs.is_subvolume(dir.path()));
        assert!(!btrfs.is_subvolume(&file));
        assert!(!btrfs.is_subvolume(&dir.path().join("absent")));
    }

    #[test]
    fn parses_the_exclusive_and_shared_columns() {
        let stdout = "     Total   Exclusive  Set shared  Filename\n  81920    4096   77824  /srv/ramet/demo/main@c1\n";
        assert_eq!(parse_summary(stdout), Some((81920, 4096, 77824)));
        assert_eq!(parse_summary("garbage\n"), None);
    }

    #[test]
    fn the_footprint_counts_shared_bytes_once() {
        let usage = Usage {
            exclusive_bytes: Some(10),
            shared_bytes: Some(90),
            ..Usage::default()
        };
        assert_eq!(usage.footprint(), Some(100));
        assert_eq!(Usage::default().footprint(), None);
    }
}
