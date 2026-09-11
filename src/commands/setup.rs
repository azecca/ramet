//! `ramet setup`: prepare the data volume, once per machine.
//!
//! Every step is skipped when already done, so running setup again is
//! harmless: it completes what is missing and changes nothing else. The steps
//! that need root (creating the mount point, adding the fstab line, handing
//! the volume over, resizing it) are shown, then run through `sudo`. This is
//! the only place ramet asks for privilege: every other command runs without
//! it.
//!
//! `--size` gives the data image its size: at creation, or afterwards to grow
//! or shrink it, online. btrfs cannot be resized without root, whatever the
//! mount options (measured), hence its place here. Neither can btrfs quotas
//! be turned on, which let `ramet df` measure every env exactly: setup turns
//! them on for ramet's image.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use crate::commands::{Outcome, prerequisites};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::layout::{DATA_IMAGE_SIZE, FSTAB, GIB, LOW_SPACE_THRESHOLD};
use crate::process::{Cmd, RunnerExt};
use crate::storage::Quotas;
use crate::storage::volume::{ImageGeometry, missing_fstab_options, missing_source_file};
use crate::ui::Ui;
use crate::util::fs::{allocated_bytes, tilde};
use crate::util::size::{human_bytes, parse_size, size_argument};

/// Arguments of `ramet setup`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Size of the data volume, as in 20G: at creation, or to grow or shrink it
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub size: Option<u64>,
}

/// Room a shrunk volume keeps above what it holds, for btrfs to move data
/// around and for its metadata to grow.
const SHRINK_MARGIN: u64 = GIB;

/// Width of the step labels, so that their details line up.
const LABEL_WIDTH: usize = 14;

/// Indentation of the lines shown under a step's details.
const DETAIL_INDENT: usize = 2 + 2 + LABEL_WIDTH + 1;

/// The comment written above the fstab line, for whoever reads the file next.
const FSTAB_COMMENT: &str = "# ramet data volume: mounted on demand by its user, never at boot";

/// Runs `ramet setup`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let out = Steps::new(ctx);
    out.title();
    let Some(complete) = check_prerequisites(ctx, &out) else {
        return Ok(Outcome::Exit(1));
    };
    let size = args.size.unwrap_or(DATA_IMAGE_SIZE);
    if !ctx.data_volume().is_usable() && !prepare_volume(ctx, &out, size)? {
        out.ui.blank();
        return Ok(Outcome::Exit(1));
    }
    if let Some(size) = args.size
        && !resize_volume(ctx, &out, size)?
    {
        out.ui.blank();
        return Ok(Outcome::Exit(1));
    }
    if !ensure_quotas(ctx, &out)? {
        out.ui.blank();
        return Ok(Outcome::Exit(1));
    }
    report_volume(ctx, &out);
    out.conclude(complete);
    Ok(if complete {
        Outcome::Done
    } else {
        Outcome::Exit(1)
    })
}

/// Reports the prerequisites. Returns `None` when the data volume cannot be
/// prepared, otherwise whether nothing at all is missing.
fn check_prerequisites(ctx: &Context, out: &Steps<'_>) -> Option<bool> {
    let checks = prerequisites::check(ctx);
    let (met, unmet): (Vec<_>, Vec<_>) = checks.iter().partition(|check| check.met);
    let names = |checks: &[&prerequisites::Check]| {
        checks
            .iter()
            .map(|check| check.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if unmet.is_empty() {
        out.done("prerequisites", names(&met));
        return Some(true);
    }
    out.failed("prerequisites", format!("missing: {}", names(&unmet)));
    for check in &unmet {
        out.detail(out.ui.style().red(&check.message));
        if let Some(hint) = check.hint {
            out.detail(hint);
        }
    }
    if unmet.iter().any(|check| check.for_volume) {
        out.ui.blank();
        out.ui
            .out("  Install what is missing, then run `ramet setup` again.");
        return None;
    }
    Some(false)
}

/// Brings the data volume to a usable state, creating the image with `size`
/// bytes if there is none. Returns false when the user has to run the
/// privileged steps by hand.
fn prepare_volume(ctx: &Context, out: &Steps<'_>, size: u64) -> Result<bool> {
    let volume = ctx.data_volume();
    let root = ctx.layout().root().to_owned();
    let mounted = volume.mounted_filesystem();
    if mounted.is_empty() {
        let privileged = prepare_mount(ctx, out, size)?;
        if !run_as_root(ctx, out, "system", None, &privileged)? {
            return Ok(false);
        }
    } else if mounted != "btrfs" {
        return Err(Error::ForeignMount {
            root,
            filesystem: mounted,
        });
    }
    match volume.mount_if_needed() {
        Ok(_) => Ok(true),
        // Mounted, but its root belongs to someone else: an image formatted
        // without `--rootdir`, or by an older btrfs-progs.
        Err(Error::RootNotWritable { root }) => {
            let hand_over = RootStep::HandOver {
                root,
                user: ctx.host().user_name(),
            };
            if !run_as_root(ctx, out, "system", None, &[hand_over])? {
                return Ok(false);
            }
            volume.mount_if_needed().map(|_| true)
        }
        Err(err) => Err(err),
    }
}

/// Prepares what an unmounted volume needs: the image and its fstab line.
/// Returns the steps left to root.
fn prepare_mount(ctx: &Context, out: &Steps<'_>, size: u64) -> Result<Vec<RootStep>> {
    let volume = ctx.data_volume();
    let layout = ctx.layout();
    let root = layout.root();
    let mut privileged = Vec::new();
    if !root.is_dir() {
        privileged.push(RootStep::CreateMountPoint(root.to_owned()));
    }
    let source = volume.fstab_source();
    if source.is_empty() {
        ensure_image(ctx, out, size)?;
        privileged.push(RootStep::AddFstabLine(layout.fstab_line()));
        return Ok(privileged);
    }
    // An existing line is used as it is, or refused: ramet never edits one.
    let missing = missing_fstab_options(&volume.fstab_options());
    if !missing.is_empty() {
        return Err(Error::FstabLineIncomplete {
            root: root.to_owned(),
            missing,
        });
    }
    if volume.fstab_mounts_the_image(&source) {
        ensure_image(ctx, out, size)?;
        out.done("/etc/fstab", "line present");
    } else if let Some(source_file) = missing_source_file(&source) {
        return Err(Error::FstabSourceMissing {
            root: root.to_owned(),
            source_file,
        });
    } else {
        out.done("/etc/fstab", format!("line present, mounts {source}"));
    }
    Ok(privileged)
}

/// Creates the data image with `size` bytes unless it exists.
fn ensure_image(ctx: &Context, out: &Steps<'_>, size: u64) -> Result<()> {
    let image = ctx.layout().data_image();
    if let Ok(meta) = image.metadata() {
        out.done(
            "data image",
            format!(
                "{}: {} announced, {} used",
                out.path(image),
                human_bytes(Some(meta.len())),
                human_bytes(Some(allocated_bytes(&meta)))
            ),
        );
        return Ok(());
    }
    ctx.data_volume().create_image(size)?;
    out.done(
        "data image",
        format!(
            "{} created: {}, sparse",
            out.path(image),
            human_bytes(Some(size))
        ),
    );
    Ok(())
}

/// Brings the mounted data volume to `size` bytes. Returns false when the
/// user has to run the privileged steps by hand.
fn resize_volume(ctx: &Context, out: &Steps<'_>, size: u64) -> Result<bool> {
    let volume = ctx.data_volume();
    let root = ctx.layout().root();
    let Some(geometry) = volume.image_geometry() else {
        return Err(Error::VolumeNotResizable {
            root: root.to_owned(),
            mounted_from: volume.source(),
        });
    };
    let steps = resize_steps(&geometry, size, ctx.layout().data_image(), root);
    if steps.is_empty() {
        return Ok(true);
    }
    if size < geometry.filesystem {
        let used = volume.space()?.used();
        let minimum = (used + SHRINK_MARGIN).div_ceil(1 << 20) << 20;
        if size < minimum {
            return Err(Error::VolumeTooSmall {
                requested: size,
                used,
                minimum,
            });
        }
    }
    let intro = format!(
        "{} → {}",
        human_bytes(Some(geometry.filesystem)),
        human_bytes(Some(size))
    );
    if !run_as_root(ctx, out, "resize", Some(&intro), &steps)? {
        return Ok(false);
    }
    let image_dir = ctx.layout().data_image().parent().unwrap_or(Path::new("/"));
    if let Ok(free) = ctx.host().free_bytes(image_dir)
        && size.saturating_sub(geometry.on_disk) > free
    {
        out.detail(out.ui.style().yellow(format!(
            "the host disk has {} left: if the image fills it, btrfs turns read-only",
            human_bytes(Some(free))
        )));
    }
    Ok(true)
}

/// Turns btrfs quotas on for ramet's image, or has their counts redone when
/// btrfs no longer trusts them. Returns false when the user has to run the
/// privileged step by hand.
///
/// Only on ramet's image: on a btrfs of the user's own, quotas would count
/// the whole filesystem, which is theirs to decide.
fn ensure_quotas(ctx: &Context, out: &Steps<'_>) -> Result<bool> {
    let volume = ctx.data_volume();
    if volume.image_device().is_none() {
        return Ok(true);
    }
    let root = ctx.layout().root().to_owned();
    let steps = match volume.quotas() {
        Quotas::Unknown => return Ok(true),
        Quotas::Enabled {
            consistent: true, ..
        } => {
            out.done("quotas", "on: `ramet df` measures every env exactly");
            return Ok(true);
        }
        Quotas::Enabled {
            consistent: false, ..
        } => vec![RootStep::RescanQuotas(root)],
        Quotas::Disabled => vec![
            RootStep::EnableQuotas(root.clone()),
            RootStep::RescanQuotas(root),
        ],
    };
    run_as_root(
        ctx,
        out,
        "quotas",
        Some("exact sizes for `ramet df`"),
        &steps,
    )
}

/// What brings the image, its loop device and the btrfs on it to `size`
/// bytes, in order. Empty when they are there already.
///
/// Growing goes from the bottom up: the image, then the loop device, then
/// btrfs. Shrinking starts with btrfs, which moves the data out of the end of
/// the device and may refuse: the image is only cut once nothing lives there.
/// The same order completes a resize that was interrupted.
fn resize_steps(geometry: &ImageGeometry, size: u64, image: &Path, root: &Path) -> Vec<RootStep> {
    let resize_btrfs = || RootStep::ResizeFilesystem {
        root: root.to_owned(),
        size,
    };
    let mut steps = Vec::new();
    if size < geometry.filesystem {
        steps.push(resize_btrfs());
    }
    if geometry.file != size {
        steps.push(RootStep::ResizeImage {
            image: image.to_owned(),
            size,
        });
    }
    if geometry.device_size != size {
        steps.push(RootStep::ReloadDevice(geometry.device.clone()));
    }
    if size > geometry.filesystem {
        steps.push(resize_btrfs());
    }
    steps
}

/// Shows `steps` on a line labelled `label`, after `intro` when there is
/// one, then runs them as root. Returns false when running them is not
/// possible and the user has to do it by hand.
fn run_as_root(
    ctx: &Context,
    out: &Steps<'_>,
    label: &str,
    intro: Option<&str>,
    steps: &[RootStep],
) -> Result<bool> {
    let intro = intro.map(|intro| format!("{intro}; ")).unwrap_or_default();
    if steps.is_empty() {
        return Ok(true);
    }
    let host = ctx.host();
    // A genuinely root environment (a container, a machine administered that
    // way) needs no sudo; `sudo ramet setup` never gets this far.
    let through_sudo = host.effective_uid() != 0;
    if through_sudo && host.find_program("sudo").is_none() {
        out.failed(
            label,
            format!("{intro}root is needed, and sudo is not installed."),
        );
        out.detail("As root, run:");
        out.commands(steps);
        out.detail("then run the same `ramet setup` again.");
        return Ok(false);
    }
    let how = if through_sudo {
        "needs root once; sudo will run:"
    } else {
        "running as root:"
    };
    out.pending(label, format!("{intro}{how}"));
    out.commands(steps);
    for step in steps {
        step.check(ctx)?;
        ctx.runner().run_checked(&step.command(through_sudo))?;
    }
    let done: Vec<String> = steps.iter().map(RootStep::outcome).collect();
    out.done(label, done.join(", "));
    Ok(true)
}

/// Reports the usable data volume.
fn report_volume(ctx: &Context, out: &Steps<'_>) {
    let root = ctx.layout().root().display();
    let Ok(space) = ctx.data_volume().space() else {
        out.done("data volume", format!("{root} ready"));
        return;
    };
    out.done(
        "data volume",
        format!(
            "{root} ready: {}, {} free",
            human_bytes(Some(space.size)),
            human_bytes(Some(space.available))
        ),
    );
    if space.available < LOW_SPACE_THRESHOLD {
        out.detail(out.ui.style().yellow(format!(
            "less than {} free: every command will warn about it",
            human_bytes(Some(LOW_SPACE_THRESHOLD))
        )));
    }
}

/// An operation only root can perform.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RootStep {
    /// Create the mount point of the data volume.
    CreateMountPoint(PathBuf),
    /// Append the data volume's line to `/etc/fstab`.
    AddFstabLine(String),
    /// Give the root of the mounted volume to the user.
    HandOver {
        /// The data root.
        root: PathBuf,
        /// The user.
        user: String,
    },
    /// Resize the btrfs mounted on the data root, online.
    ResizeFilesystem {
        /// The data root.
        root: PathBuf,
        /// The new size, in bytes.
        size: u64,
    },
    /// Grow or cut the image file. It belongs to the user, who could do it
    /// alone, but it sits between root steps: as root, the whole sequence
    /// runs in one go, and a root shell can paste it as it is.
    ResizeImage {
        /// The image.
        image: PathBuf,
        /// The new size, in bytes.
        size: u64,
    },
    /// Make the loop device read the image's new size.
    ReloadDevice(String),
    /// Turn btrfs quotas on for the data volume.
    EnableQuotas(PathBuf),
    /// Count btrfs quotas again, and wait for the count to finish.
    RescanQuotas(PathBuf),
}

impl RootStep {
    /// The command performing the step.
    fn command(&self, through_sudo: bool) -> Cmd {
        let program = |name: &str| {
            if through_sudo {
                Cmd::new("sudo").arg(name)
            } else {
                Cmd::new(name)
            }
        };
        match self {
            Self::CreateMountPoint(root) => program("mkdir").arg("-p").arg(root),
            // A leading newline: the file may not end with one, and a line
            // glued to the previous one would corrupt both.
            Self::AddFstabLine(line) => program("tee")
                .args(["-a", FSTAB])
                .input(format!("\n{FSTAB_COMMENT}\n{line}")),
            Self::HandOver { root, user } => program("chown").arg(user).arg(root),
            Self::ResizeFilesystem { root, size } => program("btrfs")
                .args(["filesystem", "resize", &size_argument(*size)])
                .arg(root),
            Self::ResizeImage { image, size } => program("truncate")
                .args(["-s", &size_argument(*size)])
                .arg(image),
            Self::ReloadDevice(device) => program("losetup").args(["-c", device]),
            Self::EnableQuotas(root) => program("btrfs").args(["quota", "enable"]).arg(root),
            Self::RescanQuotas(root) => program("btrfs").args(["quota", "rescan", "-w"]).arg(root),
        }
    }

    /// Refuses to run the step when what it relies on does not hold.
    ///
    /// Cutting the image below the btrfs it holds would lose whatever lives
    /// at the end: before cutting, btrfs must already be no larger, which the
    /// failure of `btrfs filesystem resize` would otherwise not guarantee.
    fn check(&self, ctx: &Context) -> Result<()> {
        let Self::ResizeImage { size, .. } = self else {
            return Ok(());
        };
        let filesystem = ctx.data_volume().space()?.size;
        if filesystem > *size {
            return Err(Error::ShrinkIncomplete {
                filesystem,
                requested: *size,
            });
        }
        Ok(())
    }

    /// The step as a root shell would run it, for the user to read or paste.
    fn shell_lines(&self) -> Vec<String> {
        match self {
            Self::CreateMountPoint(root) => vec![format!("mkdir -p {}", root.display())],
            Self::AddFstabLine(line) => vec![
                format!("cat >> {FSTAB} <<'EOF'"),
                FSTAB_COMMENT.to_owned(),
                line.trim_end().to_owned(),
                "EOF".to_owned(),
            ],
            Self::HandOver { root, user } => vec![format!("chown {user} {}", root.display())],
            Self::ResizeFilesystem { .. }
            | Self::ResizeImage { .. }
            | Self::ReloadDevice(_)
            | Self::EnableQuotas(_)
            | Self::RescanQuotas(_) => vec![self.command(false).to_string()],
        }
    }

    /// What the step achieved, once run.
    fn outcome(&self) -> String {
        match self {
            Self::CreateMountPoint(root) => format!("{} created", root.display()),
            Self::AddFstabLine(_) => format!("{FSTAB} line added"),
            Self::HandOver { root, user } => format!("{} handed over to {user}", root.display()),
            Self::ResizeFilesystem { size, .. } => {
                format!("btrfs resized to {}", human_bytes(Some(*size)))
            }
            Self::ResizeImage { size, .. } => {
                format!("image set to {}", human_bytes(Some(*size)))
            }
            Self::ReloadDevice(device) => format!("{device} reloaded"),
            Self::EnableQuotas(_) => "quotas on".to_owned(),
            Self::RescanQuotas(_) => "counted".to_owned(),
        }
    }
}

/// Prints the steps of the setup as aligned lines.
struct Steps<'a> {
    ui: &'a Ui,
    home: Option<PathBuf>,
}

impl<'a> Steps<'a> {
    fn new(ctx: &'a Context) -> Self {
        Self {
            ui: ctx.ui(),
            home: ctx.host().var("HOME").map(PathBuf::from),
        }
    }

    fn title(&self) {
        let style = self.ui.style();
        self.ui.blank();
        self.ui.out(format!(
            "  {}  {}",
            style.bold("ramet setup"),
            style.dim("prepares the data volume, once per machine")
        ));
        self.ui.blank();
    }

    fn step(&self, mark: &str, label: &str, detail: impl Display) {
        let label = self.ui.style().bold(format!("{label:<LABEL_WIDTH$}"));
        self.ui.out(format!("  {mark} {label} {detail}"));
    }

    fn done(&self, label: &str, detail: impl Display) {
        self.step(&self.ui.style().green("✓"), label, detail);
    }

    fn pending(&self, label: &str, detail: impl Display) {
        self.step(&self.ui.style().cyan("·"), label, detail);
    }

    fn failed(&self, label: &str, detail: impl Display) {
        self.step(&self.ui.style().red("✗"), label, detail);
    }

    /// A line under the details of the last step.
    fn detail(&self, text: impl Display) {
        self.ui.out(format!("{:DETAIL_INDENT$}{text}", ""));
    }

    /// The shell lines of `steps`, under the details of the last step.
    fn commands(&self, steps: &[RootStep]) {
        let style = self.ui.style();
        for line in steps.iter().flat_map(RootStep::shell_lines) {
            self.detail(format!("  {}", style.cyan(line)));
        }
    }

    /// The closing words: what to do next.
    fn conclude(&self, complete: bool) {
        let style = self.ui.style();
        self.ui.blank();
        if complete {
            self.ui.out(format!(
                "  {} In the main clone of a docker compose project, run:",
                style.green(style.bold("ramet is ready."))
            ));
            self.ui.blank();
            self.ui.out(format!("      {}", style.bold("ramet init")));
        } else {
            self.ui.out(format!(
                "  {} Install what is missing above, then check with `ramet doctor`.",
                style.yellow(style.bold("The data volume is ready."))
            ));
        }
        self.ui.blank();
    }

    /// `path`, with the home directory written `~`.
    fn path(&self, path: &Path) -> String {
        tilde(path, self.home.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fstab_line_is_appended_on_a_line_of_its_own() {
        let step = RootStep::AddFstabLine("/img /srv/ramet btrfs noauto 0 0\n".to_owned());
        let cmd = step.command(true);
        assert_eq!(cmd.argv(), ["sudo", "tee", "-a", "/etc/fstab"]);
        let input = cmd.input_text().unwrap();
        assert!(input.starts_with('\n'), "{input:?}");
        assert!(
            input.ends_with("/img /srv/ramet btrfs noauto 0 0\n"),
            "{input:?}"
        );
    }

    #[test]
    fn a_root_environment_runs_the_steps_without_sudo() {
        let step = RootStep::CreateMountPoint(PathBuf::from("/srv/ramet"));
        assert_eq!(step.command(false).argv(), ["mkdir", "-p", "/srv/ramet"]);
        assert_eq!(
            step.command(true).argv(),
            ["sudo", "mkdir", "-p", "/srv/ramet"]
        );
    }

    #[test]
    fn the_shown_lines_can_be_pasted_into_a_root_shell() {
        let lines =
            RootStep::AddFstabLine("/img /srv/ramet btrfs noauto 0 0\n".to_owned()).shell_lines();
        assert_eq!(lines.first().unwrap(), "cat >> /etc/fstab <<'EOF'");
        assert_eq!(lines.last().unwrap(), "EOF");
        assert!(lines.contains(&"/img /srv/ramet btrfs noauto 0 0".to_owned()));
    }
}
