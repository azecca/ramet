//! `ramet doctor`: check the prerequisites and report inconsistencies.
//!
//! Doctor observes and reports; it never fixes anything by itself, and never
//! mounts the data volume. It is also the only command allowed under sudo,
//! since it creates nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::commands::{Outcome, prerequisites, restore};
use crate::context::Context;
use crate::env::{Env, store};
use crate::error::Result;
use crate::layout::{
    CHECKPOINT_SEPARATOR, LOW_SPACE_THRESHOLD, Layout, checkpoint_name, env_file_name,
    is_checkpoint_name,
};
use crate::storage::Quotas;
use crate::storage::volume::{
    ImageGeometry, has_mount_option, missing_fstab_options, missing_source_file, suggested_size,
};
use crate::ui::Ui;
use crate::util::fs::{allocated_bytes, is_writable, resolve, sorted_entries};
use crate::util::size::{human_bytes, size_argument};

/// What to run when the data volume is not fully prepared.
const SETUP_ADVICE: &str = "run `ramet setup`: it prepares the data volume, once per machine";

/// Arguments of `ramet doctor`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// Print the expected /etc/fstab line on standard output, and nothing else
    #[arg(long)]
    pub print_fstab: bool,
}

/// Collects findings; only errors make doctor fail.
struct Report<'a> {
    ui: &'a Ui,
    errors: usize,
    warnings: usize,
}

impl<'a> Report<'a> {
    fn new(ui: &'a Ui) -> Self {
        Self {
            ui,
            errors: 0,
            warnings: 0,
        }
    }

    fn section(&self, title: impl AsRef<str>) {
        self.ui.blank();
        self.ui.out(self.ui.style().bold(title.as_ref()));
    }

    fn ok(&self, message: impl AsRef<str>) {
        self.ui.ok(message.as_ref());
    }

    fn info(&self, message: impl AsRef<str>) {
        self.ui.note(message.as_ref());
    }

    fn warn(&mut self, message: impl AsRef<str>) {
        self.warnings += 1;
        self.ui.warn(message.as_ref());
    }

    fn error(&mut self, message: impl AsRef<str>) {
        self.errors += 1;
        let style = self.ui.style();
        self.ui
            .out(format!("  {} {}", style.red("✗"), message.as_ref()));
    }

    fn summary(&self) -> Outcome {
        let style = self.ui.style();
        self.ui.blank();
        if self.errors > 0 {
            self.ui.out(style.red(style.bold(format!(
                "{} error(s), {} warning(s).",
                self.errors, self.warnings
            ))));
            self.ui
                .out("doctor changes nothing by itself: the decision is yours.");
            return Outcome::Exit(1);
        }
        if self.warnings > 0 {
            self.ui
                .out(style.yellow(format!("No errors, {} warning(s).", self.warnings)));
        } else {
            self.ui.out(style.green("Everything is in order."));
        }
        Outcome::Done
    }
}

/// Runs `ramet doctor`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    if args.print_fstab {
        // Raw output, meant to be redirected: nothing else on standard output.
        ctx.ui().out_raw(&ctx.layout().fstab_line());
        return Ok(Outcome::Done);
    }
    let mut report = Report::new(ctx.ui());
    check_prerequisites(ctx, &mut report);
    if check_data_volume(ctx, &mut report) {
        // Checking a project runs git and docker compose on its files, which
        // root has no business doing in a repository of the user's.
        if crate::app::elevated_from(ctx.host()).is_some() {
            report.section("Project");
            report.info("not checked under sudo: run `ramet doctor` as yourself");
        } else {
            check_project(ctx, &mut report);
        }
    }
    Ok(report.summary())
}

fn check_prerequisites(ctx: &Context, report: &mut Report<'_>) {
    report.section("Prerequisites");
    report.ok(format!("ramet {}", env!("CARGO_PKG_VERSION")));
    for check in prerequisites::check(ctx) {
        if check.met {
            report.ok(check.message);
        } else {
            report.error(check.message);
            if let Some(hint) = check.hint {
                report.info(hint);
            }
        }
    }
}

/// Reports on the data volume. Mounts nothing. Returns whether it is usable.
fn check_data_volume(ctx: &Context, report: &mut Report<'_>) -> bool {
    let layout = ctx.layout();
    let root = layout.root();
    let volume = ctx.data_volume();
    let user = ctx.host().user_name();
    report.section(format!("Data volume ({})", root.display()));

    if root.is_dir() && volume.filesystem() == "btrfs" {
        let source = volume.source();
        report.ok(format!(
            "{} mounted as btrfs (from {})",
            root.display(),
            if source.is_empty() { "?" } else { &source }
        ));
        let geometry = volume.image_geometry();
        if let Some(geometry) = &geometry {
            report_image(report, layout.data_image(), volume.free_bytes());
            check_geometry(report, geometry);
        }
        check_quotas(report, &volume.quotas(), geometry.is_some());
        let options = volume.mount_options();
        let deletable = has_mount_option(&options, "user_subvol_rm_allowed");
        let discard = has_mount_option(&options, "discard");
        if !deletable {
            report
                .error("mounted without `user_subvol_rm_allowed`: deleting a checkpoint will fail");
        }
        if !discard {
            report.warn(
                "mounted without `discard=async`: freed space will not return to the image file",
            );
        }
        if !deletable || !discard {
            report.info(format!(
                "unmount it ({0} is yours: `umount {0}`), ramet will remount it with the right options",
                root.display()
            ));
        }
        if let Ok(space) = volume.space()
            && space.available < LOW_SPACE_THRESHOLD
        {
            report.warn(format!(
                "only {} left; `ramet df` shows what takes it",
                human_bytes(Some(space.available))
            ));
            report.info(format!(
                "`ramet prune` deletes what is orphaned, `ramet setup --size {}` grows the volume",
                size_argument(suggested_size(space.size))
            ));
        }
        if is_writable(root) {
            report.ok(format!("writable by {user}"));
            return true;
        }
        report.error(format!(
            "{} does not belong to {user}: ramet cannot create project directories there",
            root.display()
        ));
        report.info("run `ramet setup`: it hands the volume over to you");
        return false;
    }

    let mounted = volume.mounted_filesystem();
    if !mounted.is_empty() && mounted != "btrfs" {
        report.error(format!(
            "{} holds a {mounted} mount, not btrfs",
            root.display()
        ));
        return false;
    }

    // Not mounted: ramet mounts it by itself, provided everything the fstab
    // line refers to is in place.
    let options = volume.fstab_options();
    if options.is_empty() {
        report.error(format!(
            "{} is not mounted and no /etc/fstab line describes it",
            root.display()
        ));
        report.info(SETUP_ADVICE);
        return false;
    }
    let errors = report.errors;
    for option in missing_fstab_options(&options) {
        report.error(format!("the /etc/fstab line lacks `{option}`"));
    }
    if !root.is_dir() {
        report.error(format!("the mount point {} does not exist", root.display()));
    }
    if let Some(file) = missing_source_file(&volume.fstab_source()) {
        report.error(format!(
            "the /etc/fstab line mounts {}, which does not exist",
            file.display()
        ));
    }
    if report.errors == errors {
        report.ok("/etc/fstab line present: ramet will mount the volume at the next command");
        if layout.data_image().exists() {
            report_image(report, layout.data_image(), None);
        }
    } else {
        report.info(SETUP_ADVICE);
    }
    false
}

/// Announced and occupied size of the data image, and the space left inside.
fn report_image(report: &Report<'_>, image: &Path, free_inside: Option<u64>) {
    let Ok(meta) = image.metadata() else {
        return;
    };
    let inside = free_inside
        .map(|free| format!(", {} free inside", human_bytes(Some(free))))
        .unwrap_or_default();
    report.ok(format!(
        "image {}: {} announced, {} used on disk{inside}",
        image.display(),
        human_bytes(Some(meta.len())),
        human_bytes(Some(allocated_bytes(&meta)))
    ));
}

/// Reports whether `ramet df` can measure every env exactly.
fn check_quotas(report: &mut Report<'_>, quotas: &Quotas, own_image: bool) {
    match quotas {
        Quotas::Unknown => {}
        Quotas::Enabled {
            consistent: true, ..
        } => report.ok("btrfs quotas on: `ramet df` measures every env exactly"),
        Quotas::Enabled {
            consistent: false, ..
        } => {
            report.warn("btrfs quota counts are inconsistent: `ramet df` may be off");
            report.info("`ramet setup` has them recounted");
        }
        Quotas::Disabled if own_image => report
            .info("btrfs quotas off: `ramet df` shows lower bounds; `ramet setup` turns them on"),
        Quotas::Disabled => report.info("btrfs quotas off: `ramet df` shows lower bounds"),
    }
}

/// Reports an image, loop device and btrfs of different sizes: a resize that
/// did not finish.
fn check_geometry(report: &mut Report<'_>, geometry: &ImageGeometry) {
    if geometry.is_consistent() {
        return;
    }
    let sizes = format!(
        "image {}, loop device {}, btrfs {}",
        human_bytes(Some(geometry.file)),
        human_bytes(Some(geometry.device_size)),
        human_bytes(Some(geometry.filesystem))
    );
    if geometry.file < geometry.filesystem {
        report.error(format!(
            "the image is smaller than the btrfs it holds ({sizes}): whatever btrfs kept at its end is unreachable"
        ));
        report.info(format!(
            "give the image its size back: `ramet setup --size {}`",
            size_argument(geometry.filesystem)
        ));
        return;
    }
    report.warn(format!("a resize did not finish: {sizes}"));
    report.info(format!(
        "run the `ramet setup --size` that was interrupted again: `--size {}` grows btrfs to \
         the image, `--size {}` cuts the image to btrfs",
        size_argument(geometry.file),
        size_argument(geometry.filesystem)
    ));
}

/// Reports inconsistencies in the envs of the current project.
fn check_project(ctx: &Context, report: &mut Report<'_>) {
    let location = store::locate(ctx);
    let Some(worktree) = location.worktree else {
        report.section("Project");
        report.error(format!(
            "{} is not inside a git repository",
            ctx.cwd().display()
        ));
        return;
    };
    let Some(project) = location.project else {
        report.section("Project");
        report.info(format!("current worktree: {}", worktree.display()));
        report.info("no known env for this repository: run `ramet init` in the main clone");
        return;
    };

    let layout = ctx.layout();
    report.section(format!("Project \"{project}\""));
    report.info(format!("data: {}", layout.project_dir(&project).display()));

    let envs = store::load_envs(layout, &project);
    let subvolumes = store::subvolumes(ctx, &project);
    let names: BTreeSet<String> = subvolumes
        .iter()
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();

    for path in &subvolumes {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if is_checkpoint_name(&name) || envs.contains_key(name.as_ref()) {
            continue;
        }
        if path.join(env_file_name()).is_file() {
            report.error(format!(
                "unreadable env.json: {}",
                path.join(env_file_name()).display()
            ));
        } else {
            report.error(format!("subvolume without env.json: {}", path.display()));
        }
    }

    let recorded: BTreeSet<String> = envs
        .values()
        .flat_map(|env| {
            env.checkpoints
                .keys()
                .map(|label| checkpoint_name(&env.name, label))
        })
        .collect();
    for name in names.iter().filter(|name| is_checkpoint_name(name)) {
        if let Some((env_name, label)) = name.split_once(CHECKPOINT_SEPARATOR)
            && [restore::INCOMING_LABEL, restore::OUTGOING_LABEL].contains(&label)
        {
            report_restore_leftover(report, layout, &project, env_name, label, &names);
            continue;
        }
        if !recorded.contains(name) {
            report.warn(format!(
                "checkpoint on disk but missing from env.json: {name}"
            ));
        }
    }
    for name in recorded.difference(&names) {
        report.error(format!(
            "checkpoint recorded in env.json but missing from disk: {name}"
        ));
    }

    if envs.is_empty() {
        report.warn(format!(
            "no env.json under {}",
            layout.project_dir(&project).display()
        ));
        return;
    }

    let live: BTreeSet<PathBuf> = ctx
        .git()
        .worktrees(&worktree)
        .iter()
        .map(|tree| resolve(&tree.path))
        .collect();
    for env in envs.values() {
        check_env(ctx, report, env, &live);
    }
}

/// Reports what a `ramet restore` cut short left beside the env `env_name`:
/// a copy of a checkpoint never swapped in, or the data it replaced.
fn report_restore_leftover(
    report: &mut Report<'_>,
    layout: &Layout,
    project: &str,
    env_name: &str,
    label: &str,
    names: &BTreeSet<String>,
) {
    let leftover = layout.env_dir(project, &checkpoint_name(env_name, label));
    let env_dir = layout.env_dir(project, env_name);
    if label == restore::OUTGOING_LABEL && !names.contains(env_name) {
        report.error(format!(
            "a restore was cut short while swapping the data of \"{env_name}\": `mv {} {}` puts it back",
            leftover.display(),
            env_dir.display()
        ));
    } else {
        report.warn(format!(
            "left by a restore of \"{env_name}\" cut short: {}; the next restore deletes it",
            leftover.display()
        ));
    }
}

/// Reports on one env: its subvolume, its worktree and its volumes.
fn check_env(ctx: &Context, report: &mut Report<'_>, env: &Env, live: &BTreeSet<PathBuf>) {
    let layout = ctx.layout();
    report.section(format!("  env \"{}\"", env.name));
    let dir = env.dir(layout);
    if ctx.subvolumes().is_subvolume(&dir) {
        report.ok(format!("subvolume {}", dir.display()));
    } else {
        report.error(format!("{} is not a btrfs subvolume", dir.display()));
    }
    // What a process killed while the stack was frozen leaves: every client
    // of the env hangs without a word.
    let paused: Vec<String> = ctx
        .compose()
        .containers(&env.compose_project())
        .into_iter()
        .filter(|container| container.state == "paused")
        .map(|container| container.service)
        .collect();
    if !paused.is_empty() {
        report.error(format!(
            "paused, left frozen by a command that did not finish: {}; \
             `ramet compose unpause` in the env's worktree releases them",
            paused.join(", ")
        ));
    }

    if !env.worktree.is_dir() {
        report.error(format!("worktree gone: {}", env.worktree.display()));
        return;
    }
    if !live.contains(&resolve(&env.worktree)) {
        report.error(format!(
            "{} exists but git no longer knows it as a worktree",
            env.worktree.display()
        ));
        return;
    }
    let branch = ctx
        .git()
        .current_branch(&env.worktree)
        .unwrap_or_else(|| "detached".to_owned());
    report.ok(format!(
        "worktree {} (branch {branch})",
        env.worktree.display()
    ));

    let config = match env.resolve(ctx, &[]) {
        Ok(config) => config,
        Err(err) => {
            report.error(err.to_string());
            return;
        }
    };
    let fixed = config.fixed_container_names();
    if !fixed.is_empty() {
        let services: Vec<&str> = fixed.keys().map(String::as_str).collect();
        report.info(format!(
            "`container_name` ({}) is removed from the generated configuration: \
             otherwise two envs would want the same container",
            services.join(", ")
        ));
    }

    let volumes = config.declared_volumes();
    let volumes_dir = layout.volumes_dir(&env.project, &env.name);
    let on_disk: BTreeSet<String> = sorted_entries(&volumes_dir)
        .into_iter()
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    for volume in &volumes.external {
        report.warn(format!(
            "volume `{volume}` is external: left as it is, not isolated by ramet"
        ));
    }
    for volume in volumes.managed.difference(&on_disk) {
        report.info(format!(
            "volume `{volume}` declared without a directory: created at the next `ramet compose up`"
        ));
    }
    for volume in on_disk
        .iter()
        .filter(|volume| !volumes.managed.contains(*volume) && !volumes.external.contains(*volume))
    {
        report.warn(format!(
            "orphan directory `{volume}`: no volume of that name in the compose file anymore ({})",
            volumes_dir.join(volume).display()
        ));
    }
    if !volumes.managed.is_empty() && volumes.managed.is_subset(&on_disk) {
        report.ok(format!(
            "{} named volume(s) present on disk",
            volumes.managed.len()
        ));
    }
}
