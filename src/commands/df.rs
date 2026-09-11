//! `ramet df`: how full the data volume is, and what fills it.
//!
//! The volume's figures come from the filesystem and are exact. For each env
//! and checkpoint, `df` reports what it holds and what it holds alone, the
//! rest being shared with its clones and checkpoints: exactly when btrfs
//! quotas are on, as lower bounds marked `≥` otherwise (see
//! [`Sizes`]).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::commands::Outcome;
use crate::commands::support::measured;
use crate::context::Context;
use crate::env::inventory::{self, Orphan, Project};
use crate::error::Result;
use crate::layout::LOW_SPACE_THRESHOLD;
use crate::storage::volume::suggested_size;
use crate::storage::{Quotas, Sizes};
use crate::ui::Ui;
use crate::util::fs::{tilde, to_json_pretty};
use crate::util::size::{human_bytes, size_argument};

/// Bytes of freed blocks the image must keep before `df` suggests `fstrim`:
/// below that, they are not worth a password.
const TRIM_THRESHOLD: u64 = 64 << 20;

/// Arguments of `ramet df`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct Args {
    /// JSON output, for agents: standard output holds nothing else
    #[arg(long)]
    pub json: bool,
}

/// What `df` reports.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// The data root.
    pub root: PathBuf,
    /// Size of the data volume, in bytes.
    pub size: u64,
    /// Bytes in use.
    pub used: u64,
    /// Bytes left.
    pub free: u64,
    /// ramet's image, when the volume is mounted from it.
    pub image: Option<Image>,
    /// What the volume is mounted from, when it is not ramet's image.
    pub source: Option<String>,
    /// Whether btrfs quotas are on, making every figure exact.
    pub quotas: bool,
    /// Whether btrfs trusts its quota counts; a rescan makes them right again.
    pub quotas_consistent: bool,
    /// Every project directory.
    pub projects: Vec<ProjectUsage>,
}

/// ramet's data image.
#[derive(Clone, Debug, Serialize)]
pub struct Image {
    /// The image file.
    pub path: PathBuf,
    /// Its size, which is the most the volume can hold.
    pub size: u64,
    /// What it occupies on the host disk: it is sparse.
    pub on_disk: u64,
    /// Bytes left on the host disk that holds it.
    pub host_free: Option<u64>,
}

/// One project directory.
#[derive(Clone, Debug, Serialize)]
pub struct ProjectUsage {
    /// Its name.
    pub name: String,
    /// The main clone, as its env records it.
    pub main_clone: Option<PathBuf>,
    /// Whether nothing in it belongs to anything anymore.
    pub orphaned: bool,
    /// Bytes it occupies, shared ones counted once.
    pub bytes: Option<u64>,
    /// Whether that figure is a lower bound.
    pub lower_bound: bool,
    /// Its envs and checkpoints.
    pub subvolumes: Vec<SubvolumeUsage>,
}

/// One env or checkpoint.
#[derive(Clone, Debug, Serialize)]
pub struct SubvolumeUsage {
    /// Its name: `feat-a`, or `feat-a@c1`.
    pub name: String,
    /// `env` or `checkpoint`.
    pub kind: &'static str,
    /// Its worktree, as recorded.
    pub worktree: Option<PathBuf>,
    /// Why it is orphaned: `worktree gone`, `no env.json`; `null` otherwise.
    pub orphan: Option<&'static str>,
    /// Bytes it holds, shared ones included.
    pub holds_bytes: Option<u64>,
    /// Bytes only it holds, which deleting it frees.
    pub own_bytes: Option<u64>,
    /// Whether these figures are lower bounds.
    pub lower_bound: bool,
}

/// Runs `ramet df`.
pub fn run(ctx: &Context, args: &Args) -> Result<Outcome> {
    let report = measure(ctx)?;
    if args.json {
        ctx.ui().out_raw(&to_json_pretty(&report));
    } else {
        print(ctx, &report);
    }
    Ok(Outcome::Done)
}

/// Measures the data volume and every project on it.
fn measure(ctx: &Context) -> Result<Report> {
    let volume = ctx.data_volume();
    let space = volume.space()?;
    let image = volume.image_geometry().map(|geometry| {
        let path = ctx.layout().data_image().to_owned();
        Image {
            host_free: path
                .parent()
                .and_then(|dir| ctx.host().free_bytes(dir).ok()),
            path,
            size: geometry.file,
            on_disk: geometry.on_disk,
        }
    });
    let source = image.is_none().then(|| volume.source());
    let quotas = volume.quotas();
    let sizes = Sizes::new(ctx.btrfs(), &quotas);
    let projects = inventory::scan(ctx)
        .iter()
        .map(|project| measure_project(&sizes, project))
        .collect();
    Ok(Report {
        root: ctx.layout().root().to_owned(),
        size: space.size,
        used: space.used(),
        free: space.available,
        image,
        source,
        quotas: sizes.exact(),
        quotas_consistent: !matches!(
            quotas,
            Quotas::Enabled {
                consistent: false,
                ..
            }
        ),
        projects,
    })
}

fn measure_project(sizes: &Sizes<'_>, project: &Project) -> ProjectUsage {
    let usages: Vec<_> = project
        .subvolumes
        .iter()
        .map(|subvolume| sizes.subvolume(&subvolume.path))
        .collect();
    let total = sizes.project(&project.dir, &usages);
    ProjectUsage {
        name: project.name.clone(),
        main_clone: project.main_clone().map(Path::to_owned),
        orphaned: project.is_orphaned(),
        bytes: total.bytes,
        lower_bound: total.lower_bound,
        subvolumes: project
            .subvolumes
            .iter()
            .zip(usages)
            .map(|(subvolume, usage)| SubvolumeUsage {
                name: subvolume.name.clone(),
                kind: if subvolume.checkpoint_of.is_some() {
                    "checkpoint"
                } else {
                    "env"
                },
                worktree: subvolume.worktree.clone(),
                orphan: subvolume.orphan.map(orphan_label),
                holds_bytes: usage.referenced_bytes,
                own_bytes: usage.exclusive_bytes,
                lower_bound: usage.partial,
            })
            .collect(),
    }
}

/// Why a subvolume is orphaned, in a few words.
pub fn orphan_label(orphan: Orphan) -> &'static str {
    match orphan {
        Orphan::WorktreeGone => "worktree gone",
        Orphan::NoMetadata => "no env.json",
    }
}

fn print(ctx: &Context, report: &Report) {
    let home = ctx.host().var("HOME").map(PathBuf::from);
    let path = |path: &Path| tilde(path, home.as_deref());
    print_volume(ctx.ui(), report, &path);
    print_projects(ctx.ui(), report, &path);
    print_notes(ctx.ui(), report);
}

/// The volume's own figures, and the image it lives in.
fn print_volume(ui: &Ui, report: &Report, path: &dyn Fn(&Path) -> String) {
    let style = ui.style();
    ui.out(format!("data volume {}", style.bold(report.root.display())));
    let percent = match report.used.saturating_mul(100) / report.size.max(1) {
        0 if report.used > 0 => "<1".to_owned(),
        percent => percent.to_string(),
    };
    ui.out(format!(
        "  size     {}, {} used ({percent}%), {} free",
        human_bytes(Some(report.size)),
        human_bytes(Some(report.used)),
        human_bytes(Some(report.free))
    ));
    if let Some(image) = &report.image {
        let host = image
            .host_free
            .map(|free| format!(", {} free there", human_bytes(Some(free))))
            .unwrap_or_default();
        ui.out(format!(
            "  image    {}: {} on the host disk{host}",
            path(&image.path),
            human_bytes(Some(image.on_disk))
        ));
        if let Some(kept) = trimmable(image.on_disk, report.used) {
            ui.note(format!(
                "about {} of it is space btrfs freed but keeps for reuse: `sudo fstrim {}` \
                 returns it to the host disk",
                human_bytes(Some(kept)),
                report.root.display()
            ));
        }
        if let Some(free) = image.host_free
            && image.size.saturating_sub(image.on_disk) > free
        {
            ui.warn(format!(
                "the image may grow to {}, more than the host disk has left: if the disk \
                 fills up, btrfs turns read-only. `ramet setup --size` lowers the ceiling.",
                human_bytes(Some(image.size))
            ));
        }
    } else if let Some(source) = report.source.as_deref().filter(|s| !s.is_empty()) {
        ui.out(format!("  source   {source}"));
    }
}

/// Bytes the image keeps on the host disk beyond what btrfs uses, when they
/// are worth handing back.
///
/// `discard=async` hands freed space back to the host disk by itself, but
/// only in large enough pieces: blocks freed one by one, such as metadata
/// nodes, stay in the image for btrfs to reuse. They add up (measured: an
/// image of 116 MiB for 6 MiB in use, down to 1.4 MiB after `fstrim`).
fn trimmable(on_disk: u64, used: u64) -> Option<u64> {
    let kept = on_disk.saturating_sub(used);
    (kept >= TRIM_THRESHOLD && kept.saturating_mul(4) >= on_disk).then_some(kept)
}

/// Every project, and what each env and checkpoint holds.
fn print_projects(ui: &Ui, report: &Report, path: &dyn Fn(&Path) -> String) {
    let style = ui.style();
    let holds = |s: &SubvolumeUsage| measured(s.holds_bytes, s.lower_bound);
    let subvolumes = || report.projects.iter().flat_map(|p| &p.subvolumes);
    let width = subvolumes()
        .map(|subvolume| subvolume.name.chars().count())
        .max()
        .unwrap_or(0);
    let holds_width = subvolumes()
        .map(|s| holds(s).chars().count())
        .max()
        .unwrap_or(0);
    for project in &report.projects {
        ui.blank();
        let whereabouts = if project.orphaned {
            style.yellow("orphaned: every worktree is gone")
        } else {
            project
                .main_clone
                .as_deref()
                .map(|clone| style.dim(path(clone)))
                .unwrap_or_default()
        };
        ui.out(format!(
            "project {}   {}   {whereabouts}",
            style.bold(&project.name),
            measured(project.bytes, project.lower_bound)
        ));
        for subvolume in &project.subvolumes {
            let orphan = subvolume
                .orphan
                .map(|reason| {
                    let worktree = subvolume
                        .worktree
                        .as_deref()
                        .map(|w| format!(": {}", path(w)))
                        .unwrap_or_default();
                    format!("   {}", style.yellow(format!("{reason}{worktree}")))
                })
                .unwrap_or_default();
            ui.out(format!(
                "  {:width$}   holds {:>holds_width$}   own {}{orphan}",
                subvolume.name,
                holds(subvolume),
                measured(subvolume.own_bytes, subvolume.lower_bound)
            ));
        }
    }
}

/// What the figures mean, and what to do next.
fn print_notes(ui: &Ui, report: &Report) {
    let style = ui.style();
    let subvolumes = || report.projects.iter().flat_map(|p| &p.subvolumes);
    ui.blank();
    ui.out(style.dim(
        "own: what only this env or checkpoint holds, which deleting it frees; \
         clones share the rest",
    ));
    if report.projects.iter().any(|p| p.lower_bound) || subvolumes().any(|s| s.lower_bound) {
        ui.out(style.dim("≥: at least that much"));
    }
    if !report.quotas && subvolumes().any(|s| s.lower_bound) {
        let fix = if report.image.is_some() {
            "`ramet setup` turns btrfs quotas on for exact figures"
        } else {
            "btrfs quotas on that filesystem would make them exact"
        };
        ui.out(style.dim(format!(
            "   directories unreadable without root (a database's, for one) are left out: {fix}"
        )));
    }
    if !report.quotas_consistent {
        ui.out(
            style.yellow("btrfs quota counts are inconsistent: `ramet setup` has them recounted"),
        );
    }
    let orphans = subvolumes().filter(|s| s.orphan.is_some()).count();
    if orphans > 0 {
        ui.out(format!(
            "{orphans} orphaned subvolume(s): `ramet prune` lists them and offers to delete them"
        ));
    }
    if report.image.is_some() && report.free < LOW_SPACE_THRESHOLD {
        ui.out(format!(
            "space runs low: `ramet setup --size {}` grows the volume",
            size_argument(suggested_size(report.size))
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn suggests_fstrim_only_for_a_sizeable_share_of_freed_blocks() {
        // The case measured: nearly all of the image was freed blocks.
        assert_eq!(trimmable(116 * MIB, 6 * MIB), Some(110 * MIB));
        // Metadata overhead of a working volume.
        assert_eq!(trimmable(63 * MIB, 55 * MIB), None);
        // Many freed blocks, but a small share of a large image.
        assert_eq!(trimmable(10_000 * MIB, 9_900 * MIB), None);
        // Right after `fstrim`, the image is smaller than what btrfs counts.
        assert_eq!(trimmable(MIB, 6 * MIB), None);
    }
}
