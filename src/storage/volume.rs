//! The btrfs data volume mounted on the data root.
//!
//! By default the volume is a sparse image in the user's home, described by a
//! `noauto,user` line in `/etc/fstab`: nothing is mounted at boot, and the
//! first ramet command mounts it with a plain `mount`, without privilege. Root
//! only intervenes once, when `ramet setup` prepares the machine.

use std::fs::{self, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::host::{Host, Space};
use crate::layout::{DATA_IMAGE_SIZE, GIB, LOW_SPACE_THRESHOLD, Layout};
use crate::process::{Cmd, Runner, RunnerExt};
use crate::storage::Quotas;
use crate::ui::Ui;
use crate::util::fs::{is_writable, resolve};
use crate::util::size::{human_bytes, size_argument};

/// Label of the filesystem in a data image.
const IMAGE_LABEL: &str = "ramet";

/// Size of a block as reported by `stat`.
const STAT_BLOCK_SIZE: u64 = 512;

/// Sizes of ramet's data image and of the layers built on it, in bytes.
///
/// Resizing moves them one after the other; they only differ while a resize
/// is under way, or after one was interrupted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageGeometry {
    /// The loop device the image is attached to.
    pub device: String,
    /// Size of the image file.
    pub file: u64,
    /// What the image occupies on the host disk: it is sparse.
    pub on_disk: u64,
    /// Size of the loop device, as the kernel last read it.
    pub device_size: u64,
    /// Size of the btrfs filesystem.
    pub filesystem: u64,
}

impl ImageGeometry {
    /// Whether every layer has the same size: no resize is under way.
    pub fn is_consistent(&self) -> bool {
        self.file == self.device_size && self.device_size == self.filesystem
    }
}

/// Probes and mounts the data volume.
pub struct DataVolume<'a> {
    layout: &'a Layout,
    runner: &'a dyn Runner,
    host: &'a dyn Host,
}

impl<'a> DataVolume<'a> {
    /// The data volume described by `layout`.
    pub fn new(layout: &'a Layout, runner: &'a dyn Runner, host: &'a dyn Host) -> Self {
        Self {
            layout,
            runner,
            host,
        }
    }

    fn root(&self) -> &Path {
        self.layout.root()
    }

    /// Runs `findmnt -n` with `args`, returning its trimmed output or an empty string.
    fn findmnt(&self, args: &[&str]) -> String {
        let cmd = Cmd::new("findmnt").arg("-n").args(args).arg(self.root());
        let output = self.runner.run_unchecked(&cmd);
        if output.success() {
            output.stdout_trimmed().to_owned()
        } else {
            String::new()
        }
    }

    /// Type of the filesystem that holds the root: the one mounted on it, or
    /// the one mounted above it.
    ///
    /// When nothing is mounted on the root, this reports the filesystem of its
    /// parent. That is the normal state before the first mount, hence
    /// [`mounted_filesystem`](Self::mounted_filesystem).
    pub fn filesystem(&self) -> String {
        self.findmnt(&["-o", "FSTYPE", "--target"])
    }

    /// Type of the filesystem mounted exactly on the root, or an empty string.
    pub fn mounted_filesystem(&self) -> String {
        self.findmnt(&["-o", "FSTYPE", "--mountpoint"])
    }

    /// Device or image the filesystem holding the root comes from.
    pub fn source(&self) -> String {
        self.findmnt(&["-o", "SOURCE", "--target"])
    }

    /// Whether btrfs quotas are on for the filesystem holding the root.
    pub fn quotas(&self) -> Quotas {
        // `/dev/sda2[/@home]` for a subvolume mounted from a partition.
        let source = self.source();
        let device = source.split('[').next().unwrap_or_default().trim();
        if device.is_empty() {
            return Quotas::Unknown;
        }
        Quotas::of_device(&self.host.sysfs(), Path::new(device))
    }

    /// Mount options in effect on the root, as the kernel reports them.
    pub fn mount_options(&self) -> String {
        self.findmnt(&["-o", "OPTIONS", "--mountpoint"])
    }

    /// Mount options of the `/etc/fstab` entry for the root, if there is one.
    pub fn fstab_options(&self) -> String {
        self.findmnt(&["--fstab", "-o", "OPTIONS", "--mountpoint"])
    }

    /// What the `/etc/fstab` entry for the root mounts (an image, a device,
    /// `UUID=…`), if there is one.
    pub fn fstab_source(&self) -> String {
        self.findmnt(&["--fstab", "-o", "SOURCE", "--mountpoint"])
    }

    /// Whether the `/etc/fstab` entry mounts ramet's own image.
    pub fn fstab_mounts_the_image(&self, source: &str) -> bool {
        resolve(Path::new(source)) == resolve(self.layout.data_image())
    }

    /// Whether the root sits on btrfs (mounted there or above) and belongs to the user.
    pub fn is_usable(&self) -> bool {
        self.root().is_dir() && self.filesystem() == "btrfs" && is_writable(self.root())
    }

    /// Takes away every access other users have to the data: to the image,
    /// the directory holding it, and the root of the mounted volume. Returns
    /// the paths changed.
    ///
    /// Versions up to 0.1.0 created them readable by everyone, so that any
    /// local user could copy the image, or browse the volume once mounted, and
    /// `user` in fstab lets any of them mount it. The owner's own permissions
    /// are kept; a symbolic link, or a path this user cannot change, is left
    /// as it is.
    pub fn restrict_access(&self) -> Vec<PathBuf> {
        let image = self.layout.data_image();
        let mut paths = vec![image.to_owned()];
        paths.extend(image.parent().map(Path::to_path_buf));
        if self.is_usable() {
            paths.push(self.root().to_owned());
        }
        paths
            .into_iter()
            .filter(|path| {
                let Ok(meta) = fs::symlink_metadata(path) else {
                    return false;
                };
                let mode = meta.permissions().mode();
                !meta.file_type().is_symlink()
                    && mode & 0o077 != 0
                    && fs::set_permissions(path, Permissions::from_mode(mode & !0o077)).is_ok()
            })
            .collect()
    }

    /// Bytes left in the data volume.
    pub fn free_bytes(&self) -> Option<u64> {
        self.host.free_bytes(self.root()).ok()
    }

    /// Size and free space of the data volume.
    pub fn space(&self) -> Result<Space> {
        self.host
            .space(self.root())
            .map_err(|source| Error::io(self.root(), source))
    }

    /// Mounts the data volume if needed, without any privilege.
    pub fn ensure_mounted(&self, ui: &Ui) -> Result<()> {
        if self.mount_if_needed()? {
            ui.err(format!(
                "{} {} mounted",
                ui.style().cyan("·"),
                self.root().display()
            ));
        }
        self.warn_if_low_on_space(ui);
        Ok(())
    }

    /// Mounts the data volume unless it is already usable, and checks that
    /// the user can write to it. Returns whether it had to mount it.
    pub fn mount_if_needed(&self) -> Result<bool> {
        if self.is_usable() {
            return Ok(false);
        }
        let root = self.root().to_owned();
        let mounted = self.mounted_filesystem();
        if !mounted.is_empty() && mounted != "btrfs" {
            return Err(Error::ForeignMount {
                root,
                filesystem: mounted,
            });
        }
        let mounting = mounted.is_empty();
        if mounting {
            let output = self.runner.run_unchecked(&Cmd::new("mount").arg(&root));
            if !output.success() {
                return Err(Error::NotMounted {
                    root,
                    detail: output.last_error_line().map(str::to_owned),
                });
            }
        }
        self.is_usable().ok_or(Error::RootNotWritable { root })?;
        Ok(mounting)
    }

    /// Creates the data image: a sparse file of `size` bytes, formatted as btrfs.
    ///
    /// No privilege is needed, since the image lives in the user's home. The
    /// filesystem is built from an empty directory of the user's (`--rootdir`),
    /// so that its root belongs to them from the start; without that it would
    /// belong to root, and handing it over would take a privileged mount.
    ///
    /// The image, its directory and that root are for the user alone: the
    /// volume holds every env's data, databases included. See
    /// [`restrict_access`](Self::restrict_access).
    pub fn create_image(&self, size: u64) -> Result<()> {
        let image = self.layout.data_image();
        let dir = image.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(|source| Error::io(dir, source))?;
        fs::set_permissions(dir, Permissions::from_mode(0o700))
            .map_err(|source| Error::io(dir, source))?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(image)
            .map_err(|source| Error::io(image, source))?;
        let formatted = file
            .set_len(size)
            .map_err(|source| Error::io(image, source))
            .and_then(|()| self.format_image(image, dir));
        if formatted.is_err() {
            // Half an image would pass for a real one at the next attempt.
            let _ = fs::remove_file(image);
        }
        formatted
    }

    fn format_image(&self, image: &Path, dir: &Path) -> Result<()> {
        // Everything in this directory would be copied into the new
        // filesystem: an interrupted attempt may leave it behind, but only an
        // empty one is ever reused.
        let empty = dir.join(".ramet-empty-root");
        let _ = fs::remove_dir(&empty);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&empty)
            .map_err(|source| Error::io(&empty, source))?;
        let mkfs = self
            .host
            .find_program("mkfs.btrfs")
            .unwrap_or_else(|| "mkfs.btrfs".into());
        let formatted = self.runner.run_checked(
            &Cmd::new(mkfs)
                .args(["-q", "-L", IMAGE_LABEL, "--rootdir"])
                .arg(&empty)
                .arg(image),
        );
        let _ = fs::remove_dir(&empty);
        formatted.map(drop)
    }

    /// Warns on standard error when free space runs low.
    ///
    /// Growing the image requires root, so ramet cannot do it alone; it can
    /// only say so, at every command, before space runs out.
    pub fn warn_if_low_on_space(&self, ui: &Ui) {
        let Ok(space) = self.space() else {
            return;
        };
        if space.available < LOW_SPACE_THRESHOLD {
            ui.err(format!(
                "{} only {} left in {}; `ramet df` shows what takes it, `ramet setup --size {}` grows it",
                ui.style().yellow("!"),
                human_bytes(Some(space.available)),
                self.root().display(),
                size_argument(suggested_size(space.size))
            ));
        }
    }

    /// The loop device holding the root, provided it is backed by ramet's image.
    ///
    /// A user may provide their own btrfs (a partition, a disk, a mount
    /// higher up): ramet then has nothing to say about its size.
    pub fn image_device(&self) -> Option<String> {
        let source = self.findmnt(&["-o", "SOURCE", "--mountpoint"]);
        if !source.starts_with("/dev/loop") {
            return None;
        }
        let losetup = Cmd::new("losetup").args(["-n", "-O", "BACK-FILE", &source]);
        let output = self.runner.run_unchecked(&losetup);
        let backing = output.stdout_trimmed();
        if !output.success() || backing.is_empty() {
            return None;
        }
        (resolve(Path::new(backing)) == resolve(self.layout.data_image())).then_some(source)
    }

    /// Sizes of ramet's image and of what is built on it, provided the data
    /// volume is mounted from it.
    pub fn image_geometry(&self) -> Option<ImageGeometry> {
        let device = self.image_device()?;
        let meta = self.layout.data_image().metadata().ok()?;
        // `lsblk` reads the size from sysfs: no privilege needed, unlike
        // `blockdev`, which opens the device (measured).
        let lsblk = Cmd::new("lsblk").args(["-b", "-n", "-d", "-o", "SIZE", &device]);
        let output = self.runner.run_unchecked(&lsblk);
        let device_size = output
            .success()
            .then(|| output.stdout_trimmed().parse().ok())
            .flatten()?;
        Some(ImageGeometry {
            device,
            file: meta.size(),
            on_disk: meta.blocks() * STAT_BLOCK_SIZE,
            device_size,
            filesystem: self.space().ok()?.size,
        })
    }
}

/// A size to suggest growing a volume of `size` bytes to: 10 GiB more,
/// rounded up to the GiB.
pub fn suggested_size(size: u64) -> u64 {
    (size + DATA_IMAGE_SIZE).div_ceil(GIB) * GIB
}

/// The file an fstab source names, when that file does not exist. Devices
/// (`/dev/…`, `UUID=…`, `LABEL=…`) are the administrator's business.
pub fn missing_source_file(source: &str) -> Option<PathBuf> {
    let path = Path::new(source);
    (path.is_absolute() && !path.starts_with("/dev") && !path.exists()).then(|| path.to_owned())
}

/// Options an fstab line for the data volume lacks: `user` (or `users`,
/// which also serves) for ramet to mount it without root,
/// `user_subvol_rm_allowed` for ramet to delete subvolumes without root.
pub fn missing_fstab_options(options: &str) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !["user", "users"]
        .iter()
        .any(|option| has_mount_option(options, option))
    {
        missing.push("user");
    }
    if !has_mount_option(options, "user_subvol_rm_allowed") {
        missing.push("user_subvol_rm_allowed");
    }
    missing
}

/// Whether a comma-separated option list contains `name`, alone or as `name=value`.
pub fn has_mount_option(options: &str, name: &str) -> bool {
    options.split(',').any(|option| {
        option == name
            || option
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_ten_more_gibibytes() {
        assert_eq!(suggested_size(10 * GIB), 20 * GIB);
        assert_eq!(suggested_size(10 * GIB - 4096), 20 * GIB);
    }

    #[test]
    fn mount_options_are_matched_as_whole_tokens() {
        let options = "rw,noatime,discard=async,user_subvol_rm_allowed";
        assert!(has_mount_option(options, "discard"));
        assert!(has_mount_option(options, "user_subvol_rm_allowed"));
        // `user` must not be found inside `user_subvol_rm_allowed`.
        assert!(!has_mount_option(options, "user"));
        assert!(has_mount_option("noauto,user,exec", "user"));
    }

    #[test]
    fn only_a_missing_file_counts_as_a_missing_source() {
        assert_eq!(
            missing_source_file("/nonexistent/ramet/data.img"),
            Some(PathBuf::from("/nonexistent/ramet/data.img"))
        );
        assert_eq!(missing_source_file("/"), None);
        for device in ["/dev/nonexistent", "UUID=1234-abcd", "LABEL=ramet"] {
            assert_eq!(missing_source_file(device), None, "{device}");
        }
    }

    #[test]
    fn the_fstab_line_needs_user_or_users_and_unprivileged_deletion() {
        let line = crate::layout::FSTAB_OPTIONS;
        assert!(missing_fstab_options(line).is_empty(), "{line}");
        assert!(missing_fstab_options("noauto,users,loop,user_subvol_rm_allowed").is_empty());
        // `user_subvol_rm_allowed` does not stand for `user`.
        assert_eq!(
            missing_fstab_options("noauto,loop,user_subvol_rm_allowed"),
            ["user"]
        );
        assert_eq!(
            missing_fstab_options("noauto,user,loop"),
            ["user_subvol_rm_allowed"]
        );
    }
}
