//! The data volume: mounted on demand, without privilege; warned about when full.

use std::assert_matches;
use std::cell::Cell;
use std::rc::Rc;

use ramet::error::Error;
use ramet::process::Output;

use crate::support::{Fixture, make_read_only};

const GIB: u64 = 1 << 30;

/// Answers `findmnt --target` with "btrfs" once `mount` has run.
fn mountable(fx: &Fixture) {
    let mounted = Rc::new(Cell::new(false));
    fx.runner.handle(move |cmd| {
        let argv = cmd.argv();
        match argv[0].as_str() {
            "mount" => {
                mounted.set(true);
                Some(Output::success_with(""))
            }
            "findmnt" if argv.contains(&"--target".to_owned()) => Some(if mounted.get() {
                Output::success_with("btrfs\n")
            } else {
                Output::success_with("ext4\n")
            }),
            _ => None,
        }
    });
}

#[test]
fn mounts_with_a_plain_mount() {
    let fx = Fixture::new();
    mountable(&fx);
    let ctx = fx.ctx();
    ctx.data_volume().ensure_mounted(ctx.ui()).unwrap();
    let mounts: Vec<Vec<String>> = fx
        .runner
        .argvs()
        .into_iter()
        .filter(|argv| argv[0] == "mount")
        .collect();
    assert_eq!(
        mounts,
        [vec!["mount".to_owned(), fx.root.display().to_string()]]
    );
    assert!(fx.stderr().contains("mounted"));
}

#[test]
fn does_nothing_when_already_usable() {
    let fx = Fixture::new();
    fx.runner
        .handle(|cmd| (cmd.argv()[0] == "findmnt").then(|| Output::success_with("btrfs\n")));
    let ctx = fx.ctx();
    ctx.data_volume().ensure_mounted(ctx.ui()).unwrap();
    assert!(!fx.runner.argvs().iter().any(|argv| argv[0] == "mount"));
}

#[test]
fn a_failed_mount_points_to_setup() {
    let fx = Fixture::new();
    fx.runner.handle(|cmd| {
        (cmd.argv()[0] == "mount")
            .then(|| Output::failure(1, "mount: /srv/ramet: can't find in /etc/fstab."))
    });
    let ctx = fx.ctx();
    let err = ctx.data_volume().ensure_mounted(ctx.ui()).unwrap_err();
    assert_matches!(err, Error::NotMounted { .. });
    assert!(err.to_string().contains("can't find in /etc/fstab"));
    let hint = err.hint().unwrap();
    assert!(hint.contains("ramet setup"), "{hint}");
}

#[test]
fn a_volume_owned_by_someone_else_points_to_setup() {
    let fx = Fixture::new();
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        (argv[0] == "findmnt").then(|| Output::success_with("btrfs\n"))
    });
    let ctx = fx.ctx();
    if !make_read_only(&fx.root) {
        return;
    }
    let err = ctx.data_volume().ensure_mounted(ctx.ui()).unwrap_err();
    assert_matches!(err, Error::RootNotWritable { .. });
    assert!(err.hint().unwrap().contains("ramet setup"));
}

#[test]
fn refuses_a_foreign_filesystem_on_the_mount_point() {
    let fx = Fixture::new();
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        (argv[0] == "findmnt" && argv.contains(&"--mountpoint".to_owned()))
            .then(|| Output::success_with("ext4\n"))
    });
    let ctx = fx.ctx();
    let err = ctx.data_volume().ensure_mounted(ctx.ui()).unwrap_err();
    assert_matches!(err, Error::ForeignMount { .. });
    assert!(!fx.runner.argvs().iter().any(|argv| argv[0] == "mount"));
}

#[test]
fn warns_when_space_runs_low_and_says_how_to_grow() {
    let fx = Fixture::new();
    fx.host.0.fs_size.set(10 * GIB);
    fx.host.0.free_bytes.set(GIB);
    let ctx = fx.ctx();
    ctx.data_volume().warn_if_low_on_space(ctx.ui());
    let err = fx.stderr();
    assert!(err.contains("only 1.0 GiB left"), "{err}");
    assert!(err.contains("`ramet df` shows what takes it"), "{err}");
    assert!(err.contains("`ramet setup --size 20G` grows it"), "{err}");
    assert!(
        fx.runner.argvs().is_empty(),
        "growing needs root: warning is all ramet does"
    );
}

#[test]
fn stays_silent_with_room_to_spare() {
    let fx = Fixture::new();
    let ctx = fx.ctx();
    ctx.data_volume().warn_if_low_on_space(ctx.ui());
    assert_eq!(fx.stderr(), "");
}

#[test]
fn an_image_device_survives_a_missing_losetup() {
    let fx = Fixture::new();
    fx.runner.handle(|cmd| match cmd.argv()[0].as_str() {
        "findmnt" => Some(Output::success_with("/dev/loop9\n")),
        "losetup" => Some(Output::failure(127, "losetup: not found")),
        _ => None,
    });
    assert_eq!(fx.ctx().data_volume().image_device(), None);
}

#[test]
fn recognizes_its_own_image() {
    let fx = Fixture::new();
    let image = fx.layout().data_image().to_owned();
    std::fs::write(&image, "").unwrap();
    fx.runner.handle(move |cmd| match cmd.argv()[0].as_str() {
        "findmnt" => Some(Output::success_with("/dev/loop9\n")),
        "losetup" => Some(Output::success_with(format!("{}\n", image.display()))),
        _ => None,
    });
    assert_eq!(
        fx.ctx().data_volume().image_device().as_deref(),
        Some("/dev/loop9")
    );
}
