//! How much space subvolumes take: exactly from btrfs quotas when they are
//! on, as a lower bound from `btrfs filesystem du` otherwise.
//!
//! btrfs quota groups count, for every subvolume, what it references and
//! what only it references. Turning them on takes root, once: `ramet setup`
//! does it on ramet's image. Reading them does not: the kernel publishes the
//! counts in sysfs, readable by anyone (measured). Without them, `btrfs
//! filesystem du` reads the files themselves, and cannot enter directories in
//! mode 0700, such as a database's: its figures are then lower bounds.

use std::fs;
use std::path::{Path, PathBuf};

use crate::storage::{Btrfs, Usage};

/// The state of btrfs quotas on the data volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Quotas {
    /// The volume's filesystem could not be found in sysfs.
    Unknown,
    /// Quotas are off.
    Disabled,
    /// Quotas are on.
    Enabled {
        /// The sysfs directory holding one directory per quota group.
        dir: PathBuf,
        /// Whether btrfs trusts its counts; a rescan makes them right again.
        consistent: bool,
    },
}

impl Quotas {
    /// The quota state of the btrfs filesystem mounted from `device`, as
    /// sysfs describes it under `sysfs`.
    ///
    /// Each mounted btrfs has a directory `fs/btrfs/<uuid>` listing its
    /// devices by kernel name (`loop0`, `nvme0n1p2`, `dm-0`), and a
    /// `qgroups` directory while quotas are on.
    pub fn of_device(sysfs: &Path, device: &Path) -> Self {
        // `/dev/mapper/home` is a link to `/dev/dm-0`, the kernel's name.
        let Some(name) = crate::util::fs::resolve(device)
            .file_name()
            .map(ToOwned::to_owned)
        else {
            return Self::Unknown;
        };
        let filesystem = crate::util::fs::sorted_entries(&sysfs.join("fs/btrfs"))
            .into_iter()
            .find(|dir| dir.join("devices").join(&name).exists());
        let Some(filesystem) = filesystem else {
            return Self::Unknown;
        };
        let dir = filesystem.join("qgroups");
        if !dir.is_dir() {
            return Self::Disabled;
        }
        let consistent =
            fs::read_to_string(dir.join("inconsistent")).map_or(true, |flag| flag.trim() == "0");
        Self::Enabled { dir, consistent }
    }
}

/// Bytes a project directory occupies, its subvolumes' shared data counted once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    /// The bytes, when measured.
    pub bytes: Option<u64>,
    /// Whether that figure is a lower bound.
    pub lower_bound: bool,
}

/// Measures subvolumes, from quotas when they are on.
pub struct Sizes<'a> {
    btrfs: &'a dyn Btrfs,
    qgroups: Option<PathBuf>,
}

impl<'a> Sizes<'a> {
    /// Measures through `btrfs`, and through the quota groups of `quotas`
    /// when they are on.
    pub fn new(btrfs: &'a dyn Btrfs, quotas: &Quotas) -> Self {
        let qgroups = match quotas {
            Quotas::Enabled { dir, .. } => Some(dir.clone()),
            Quotas::Unknown | Quotas::Disabled => None,
        };
        Self { btrfs, qgroups }
    }

    /// Whether subvolumes are measured exactly, from quotas.
    pub fn exact(&self) -> bool {
        self.qgroups.is_some()
    }

    /// What the subvolume at `path` holds, and holds alone.
    pub fn subvolume(&self, path: &Path) -> Usage {
        self.quota_group(path)
            .unwrap_or_else(|| self.btrfs.usage(path))
    }

    fn quota_group(&self, path: &Path) -> Option<Usage> {
        let group = self
            .qgroups
            .as_ref()?
            .join(format!("0_{}", self.btrfs.subvolume_id(path)?));
        let read = |name: &str| -> Option<u64> {
            fs::read_to_string(group.join(name))
                .ok()?
                .trim()
                .parse()
                .ok()
        };
        Some(Usage {
            referenced_bytes: Some(read("referenced")?),
            exclusive_bytes: Some(read("exclusive")?),
            shared_bytes: None,
            partial: false,
        })
    }

    /// What the project directory `dir` occupies, knowing what its
    /// subvolumes, measured by [`subvolume`](Self::subvolume), hold.
    pub fn project(&self, dir: &Path, members: &[Usage]) -> Footprint {
        if !self.exact() {
            let usage = self.btrfs.usage(dir);
            return Footprint {
                bytes: usage.footprint(),
                lower_bound: usage.partial,
            };
        }
        // Quota groups of single subvolumes cannot tell what several of them
        // share. The largest one's references, plus what each other one
        // holds alone, is a lower bound, close when the others are clones
        // and checkpoints of the largest, as in a ramet project.
        let largest = members
            .iter()
            .enumerate()
            .max_by_key(|(_, usage)| usage.referenced_bytes);
        let Some((index, largest)) = largest else {
            return Footprint {
                bytes: Some(0),
                lower_bound: false,
            };
        };
        let others: u64 = members
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .filter_map(|(_, usage)| usage.exclusive_bytes)
            .sum();
        Footprint {
            bytes: largest.referenced_bytes.map(|bytes| bytes + others),
            lower_bound: members.len() > 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(referenced: u64, exclusive: u64) -> Usage {
        Usage {
            referenced_bytes: Some(referenced),
            exclusive_bytes: Some(exclusive),
            ..Usage::default()
        }
    }

    struct NoBtrfs;

    impl Btrfs for NoBtrfs {
        fn create_subvolume(&self, _: &Path) -> crate::error::Result<()> {
            unreachable!()
        }
        fn snapshot(&self, _: &Path, _: &Path, _: bool) -> crate::error::Result<()> {
            unreachable!()
        }
        fn delete_subvolume(&self, _: &Path) -> crate::error::Result<()> {
            unreachable!()
        }
        fn is_subvolume(&self, _: &Path) -> bool {
            true
        }
        fn usage(&self, _: &Path) -> Usage {
            Usage::default()
        }
        fn subvolume_id(&self, _: &Path) -> Option<u64> {
            Some(256)
        }
    }

    fn exact() -> Sizes<'static> {
        Sizes {
            btrfs: &NoBtrfs,
            qgroups: Some(PathBuf::from("/nonexistent")),
        }
    }

    #[test]
    fn a_project_counts_the_largest_env_and_what_the_others_hold_alone() {
        let members = [usage(100, 2), usage(98, 5), usage(97, 1)];
        assert_eq!(
            exact().project(Path::new("/p"), &members),
            Footprint {
                bytes: Some(106),
                lower_bound: true,
            }
        );
    }

    #[test]
    fn a_project_of_one_subvolume_is_measured_exactly() {
        assert_eq!(
            exact().project(Path::new("/p"), &[usage(100, 100)]),
            Footprint {
                bytes: Some(100),
                lower_bound: false,
            }
        );
        assert_eq!(exact().project(Path::new("/p"), &[]).bytes, Some(0));
    }

    #[test]
    fn reads_the_state_of_quotas_from_sysfs() {
        let sysfs = tempfile::tempdir().unwrap();
        let fs_dir = sysfs.path().join("fs/btrfs/1234-abcd");
        fs::create_dir_all(fs_dir.join("devices/loop7")).unwrap();
        let loop7 = Path::new("/dev/loop7");
        assert_eq!(Quotas::of_device(sysfs.path(), loop7), Quotas::Disabled);
        assert_eq!(
            Quotas::of_device(sysfs.path(), Path::new("/dev/loop8")),
            Quotas::Unknown
        );
        fs::create_dir_all(fs_dir.join("qgroups")).unwrap();
        fs::write(fs_dir.join("qgroups/inconsistent"), "1\n").unwrap();
        assert_eq!(
            Quotas::of_device(sysfs.path(), loop7),
            Quotas::Enabled {
                dir: fs_dir.join("qgroups"),
                consistent: false,
            }
        );
    }

    #[test]
    fn reads_a_subvolume_from_its_quota_group() {
        let sysfs = tempfile::tempdir().unwrap();
        let group = sysfs.path().join("0_256");
        fs::create_dir_all(&group).unwrap();
        fs::write(group.join("referenced"), "49164288\n").unwrap();
        fs::write(group.join("exclusive"), "1163264\n").unwrap();
        let sizes = Sizes {
            btrfs: &NoBtrfs,
            qgroups: Some(sysfs.path().to_owned()),
        };
        assert_eq!(
            sizes.subvolume(Path::new("/srv/ramet/demo/main")),
            usage(49_164_288, 1_163_264)
        );
    }
}
