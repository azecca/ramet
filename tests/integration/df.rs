//! `ramet df`: the volume's exact figures, and what each project, env and
//! checkpoint holds, exactly when btrfs quotas are on.

use std::fs;

use ramet::commands::{Outcome, df};
use ramet::layout::GIB;
use ramet::storage::Usage;
use serde_json::Value;

use crate::support::{Fixture, PROJECT};

const MIB: u64 = 1 << 20;

fn run(fx: &Fixture, json: bool) -> Outcome {
    df::run(&fx.ctx(), &df::Args { json }).unwrap()
}

/// What `btrfs filesystem du` reports without quotas.
fn du(referenced: u64, exclusive: u64, shared: u64, partial: bool) -> Usage {
    Usage {
        referenced_bytes: Some(referenced),
        exclusive_bytes: Some(exclusive),
        shared_bytes: Some(shared),
        partial,
    }
}

/// A project with a main env, a checkpoint and a clone whose worktree was
/// deleted by hand, on a volume of 10 GiB with 2 GiB in use.
fn populated() -> Fixture {
    let fx = Fixture::with_main_env();
    fx.host.0.fs_size.set(10 * GIB);
    fx.host.0.free_bytes.set(8 * GIB);
    let worktree = fx.secondary_env("feat-old").worktree;
    fs::remove_dir_all(worktree).unwrap();
    let checkpoint = fx.layout().checkpoint_dir(PROJECT, "main", "c1");
    fs::create_dir_all(&checkpoint).unwrap();
    let frozen = fx.load("main");
    ramet::util::fs::write_json(&checkpoint.join("env.json"), &frozen).unwrap();
    fx
}

/// `btrfs filesystem du` figures for [`populated`], the main env's partial.
fn measured_by_du(fx: &Fixture) {
    let mut btrfs = fx.btrfs.0.borrow_mut();
    btrfs.usage = du(MIB, MIB, 0, false);
    btrfs
        .usage_of
        .insert(PROJECT.into(), du(0, 3 * MIB, 100 * MIB, false));
    btrfs
        .usage_of
        .insert("main".into(), du(100 * MIB, 5 * MIB, 0, true));
}

#[test]
fn without_quotas_reports_lower_bounds_and_how_to_get_exact_figures() {
    let fx = populated();
    measured_by_du(&fx);
    assert_eq!(run(&fx, false), Outcome::Done);
    let out = fx.stdout();
    assert!(
        out.contains("size     10.0 GiB, 2.0 GiB used (20%), 8.0 GiB free"),
        "{out}"
    );
    assert!(out.contains("project demo   103.0 MiB"), "{out}");
    assert!(
        out.contains(fx.clone.to_str().unwrap()),
        "the main clone: {out}"
    );
    assert!(
        out.contains("main       holds ≥ 100.0 MiB   own ≥ 5.0 MiB"),
        "{out}"
    );
    assert!(
        out.contains("main@c1    holds     1.0 MiB   own 1.0 MiB"),
        "{out}"
    );
    assert!(out.contains("own 1.0 MiB   worktree gone"), "{out}");
    assert!(out.contains("≥: at least that much"), "{out}");
    // Not ramet's image: quotas would count the user's whole filesystem.
    assert!(
        out.contains("btrfs quotas on that filesystem would make them exact"),
        "{out}"
    );
    assert!(
        out.contains("1 orphaned subvolume(s): `ramet prune`"),
        "{out}"
    );
    assert!(!out.contains("space runs low"), "{out}");
}

#[test]
fn with_quotas_every_env_is_measured_exactly() {
    let fx = populated();
    fx.machine().borrow_mut().mounted = true;
    fx.quotas(&[
        ("main", 100 * MIB, 4 * MIB),
        ("main@c1", 97 * MIB, MIB),
        ("feat-old", 98 * MIB, 2 * MIB),
    ]);
    // `btrfs filesystem du` would know nothing: quotas answer instead.
    fx.btrfs.0.borrow_mut().usage = du(0, 0, 0, true);

    assert_eq!(run(&fx, false), Outcome::Done);
    let out = fx.stdout();
    assert!(
        out.contains("main       holds 100.0 MiB   own 4.0 MiB"),
        "{out}"
    );
    assert!(
        out.contains("main@c1    holds  97.0 MiB   own 1.0 MiB"),
        "{out}"
    );
    // The largest env, and what each other one holds alone.
    assert!(out.contains("project demo   ≥ 103.0 MiB"), "{out}");
    assert!(!out.contains("unreadable"), "{out}");
}

#[test]
fn json_holds_every_figure() {
    let fx = populated();
    measured_by_du(&fx);
    assert_eq!(run(&fx, true), Outcome::Done);
    let report: Value = serde_json::from_str(&fx.stdout()).expect("JSON only on stdout");
    assert_eq!(report["size"], 10 * GIB);
    assert_eq!(report["used"], 2 * GIB);
    assert_eq!(report["free"], 8 * GIB);
    assert_eq!(report["quotas"], false);
    assert_eq!(report["quotas_consistent"], true);
    let project = &report["projects"][0];
    assert_eq!(project["name"], PROJECT);
    assert_eq!(project["bytes"], 103 * MIB);
    assert_eq!(project["lower_bound"], false);
    assert_eq!(project["orphaned"], false);
    let names: Vec<&str> = project["subvolumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["feat-old", "main", "main@c1"]);
    let feat = &project["subvolumes"][0];
    assert_eq!(feat["kind"], "env");
    assert_eq!(feat["orphan"], "worktree gone");
    let main = &project["subvolumes"][1];
    assert_eq!(main["holds_bytes"], 100 * MIB);
    assert_eq!(main["own_bytes"], 5 * MIB);
    assert_eq!(main["lower_bound"], true);
    assert_eq!(project["subvolumes"][2]["kind"], "checkpoint");
    assert_eq!(project["subvolumes"][2]["orphan"], Value::Null);
}

#[test]
fn a_project_whose_repository_is_gone_is_orphaned() {
    let fx = Fixture::new();
    fx.save_env(fx.env("main", |env| env.worktree = fx.base.join("vanished")));
    run(&fx, false);
    assert!(
        fx.stdout().contains("orphaned: every worktree is gone"),
        "{}",
        fx.stdout()
    );
}

#[test]
fn shows_the_image_and_warns_when_the_host_disk_cannot_hold_it() {
    let fx = Fixture::new();
    fx.image_volume(20 * GIB, 20 * GIB, 20 * GIB, GIB);
    // The same fake disk answers for the volume and for the host.
    fx.host.0.free_bytes.set(GIB);
    run(&fx, false);
    let out = fx.stdout();
    let image = fx.layout().data_image().display().to_string();
    assert!(out.contains(&format!("image    {image}: ")), "{out}");
    assert!(
        out.contains("on the host disk, 1.0 GiB free there"),
        "{out}"
    );
    assert!(
        out.contains("more than the host disk has left"),
        "a sparse image may outgrow its disk: {out}"
    );
    assert!(
        out.contains("space runs low: `ramet setup --size 30G`"),
        "{out}"
    );
}

#[test]
fn suggests_fstrim_when_the_image_keeps_freed_blocks() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, 6 * MIB);
    // What the image occupies on the host disk: 80 MiB actually written.
    let mut image = fs::OpenOptions::new()
        .write(true)
        .open(fx.layout().data_image())
        .unwrap();
    std::io::Write::write_all(&mut image, &vec![1u8; 80 << 20]).unwrap();
    image.sync_all().unwrap();

    run(&fx, false);
    let out = fx.stdout();
    assert!(
        out.contains("of it is space btrfs freed but keeps for reuse"),
        "{out}"
    );
    assert!(
        out.contains(&format!("`sudo fstrim {}`", fx.root.display())),
        "{out}"
    );
}

#[test]
fn a_sparse_image_needs_no_fstrim() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, 6 * MIB);
    run(&fx, false);
    assert!(!fx.stdout().contains("fstrim"), "{}", fx.stdout());
}
