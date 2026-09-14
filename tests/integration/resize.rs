//! `ramet setup --size`: the image, its loop device and btrfs brought to a
//! size in the one safe order, and a shrink refused before it could cut data.

use std::assert_matches;

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;
use ramet::error::Error;
use ramet::layout::GIB;

use crate::support::Fixture;

/// Runs `ramet setup --size <size>` and returns its exit code.
fn setup_size(fx: &Fixture, size: &str) -> u8 {
    let cli = Cli::try_parse_from(["ramet", "setup", "--size", size]).unwrap();
    app::run(&fx.ctx(), &cli.command)
}

/// The programs root ran through sudo, in order.
fn root_programs(fx: &Fixture) -> Vec<String> {
    fx.root_commands()
        .into_iter()
        .map(|argv| argv[1..].join(" "))
        .collect()
}

#[test]
fn grows_from_the_image_up() {
    let fx = Fixture::new();
    let image = fx.layout().data_image().display().to_string();
    let root = fx.root.display().to_string();
    let state = fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, 2 * GIB);

    assert_eq!(setup_size(&fx, "20G"), 0, "{}{}", fx.stdout(), fx.stderr());

    assert_eq!(
        root_programs(&fx),
        [
            format!("truncate -c -s 20G {image}"),
            "losetup -c /dev/loop0".to_owned(),
            format!("btrfs filesystem resize 20G {root}"),
        ]
    );
    assert_eq!(fx.image_size(), 20 * GIB);
    assert_eq!(state.borrow().device, 20 * GIB);
    assert_eq!(fx.host.0.fs_size.get(), 20 * GIB);
    let out = fx.stdout();
    assert!(out.contains("10.0 GiB → 20.0 GiB"), "{out}");
    assert!(
        out.find("losetup -c").unwrap() < out.find("btrfs resized").unwrap(),
        "the commands are shown before they run: {out}"
    );
    assert!(out.contains("ready: 20.0 GiB"), "{out}");
}

#[test]
fn shrinks_btrfs_before_cutting_the_image() {
    let fx = Fixture::new();
    let image = fx.layout().data_image().display().to_string();
    let root = fx.root.display().to_string();
    fx.image_volume(20 * GIB, 20 * GIB, 20 * GIB, 2 * GIB);

    assert_eq!(setup_size(&fx, "8G"), 0, "{}{}", fx.stdout(), fx.stderr());

    assert_eq!(
        root_programs(&fx),
        [
            format!("btrfs filesystem resize 8G {root}"),
            format!("truncate -c -s 8G {image}"),
            "losetup -c /dev/loop0".to_owned(),
        ]
    );
    assert_eq!(fx.image_size(), 8 * GIB);
}

#[test]
fn a_size_already_reached_changes_nothing() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, GIB);

    assert_eq!(setup_size(&fx, "10G"), 0);
    assert!(fx.root_commands().is_empty());
    assert!(!fx.stdout().contains("resize"), "{}", fx.stdout());
}

#[test]
fn refuses_to_shrink_below_what_the_volume_holds() {
    let fx = Fixture::new();
    fx.image_volume(20 * GIB, 20 * GIB, 20 * GIB, 7 * GIB + GIB / 2);

    assert_eq!(setup_size(&fx, "8G"), 1);
    assert!(fx.root_commands().is_empty(), "nothing runs");
    assert_eq!(fx.image_size(), 20 * GIB);
    let err = fx.stderr();
    assert!(err.contains("8.0 GiB is too small"), "{err}");
    assert!(err.contains("holds 7.5 GiB"), "{err}");
    assert!(err.contains("`ramet setup --size 9G`"), "{err}");
}

#[test]
fn a_refused_btrfs_shrink_leaves_the_image_whole() {
    let fx = Fixture::new();
    let state = fx.image_volume(20 * GIB, 20 * GIB, 20 * GIB, 2 * GIB);
    state.borrow_mut().resize_fails = true;

    assert_eq!(setup_size(&fx, "8G"), 1);
    assert_eq!(root_programs(&fx).len(), 1, "nothing after the failure");
    assert_eq!(fx.image_size(), 20 * GIB);
    assert!(fx.stderr().contains("No space left on device"));
}

#[test]
fn never_cuts_the_image_below_btrfs() {
    // `btrfs filesystem resize` claims success, yet btrfs did not shrink.
    let fx = Fixture::new();
    let state = fx.image_volume(20 * GIB, 20 * GIB, 20 * GIB, 2 * GIB);
    state.borrow_mut().resize_ignored = true;

    let ctx = fx.ctx();
    let cli = Cli::try_parse_from(["ramet", "setup", "--size", "8G"]).unwrap();
    let ramet::cli::Command::Setup(args) = cli.command else {
        panic!()
    };
    let err = ramet::commands::setup::run(&ctx, &args).unwrap_err();
    assert_matches!(err, Error::ShrinkIncomplete { .. });
    assert_eq!(fx.image_size(), 20 * GIB);
    assert!(
        !root_programs(&fx)
            .iter()
            .any(|line| line.starts_with("truncate")),
        "{:?}",
        root_programs(&fx)
    );
}

#[test]
fn completes_an_interrupted_growth() {
    // Interrupted after the image grew: the device and btrfs follow.
    let fx = Fixture::new();
    fx.image_volume(20 * GIB, 10 * GIB, 10 * GIB, GIB);

    assert_eq!(setup_size(&fx, "20G"), 0, "{}", fx.stderr());
    let programs = root_programs(&fx);
    assert_eq!(programs.len(), 2, "{programs:?}");
    assert!(programs[0].starts_with("losetup -c"));
    assert!(programs[1].starts_with("btrfs filesystem resize 20G"));
}

#[test]
fn completes_an_interrupted_shrink() {
    // Interrupted after btrfs shrank: the image and the device follow.
    let fx = Fixture::new();
    fx.image_volume(20 * GIB, 20 * GIB, 8 * GIB, GIB);

    assert_eq!(setup_size(&fx, "8G"), 0, "{}", fx.stderr());
    let programs = root_programs(&fx);
    assert_eq!(programs.len(), 2, "{programs:?}");
    assert!(programs[0].starts_with("truncate -c -s 8G"));
    assert!(programs[1].starts_with("losetup -c"));
    assert_eq!(fx.image_size(), 8 * GIB);
}

#[test]
fn without_sudo_the_whole_sequence_is_shown() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, GIB);
    fx.host
        .0
        .missing_programs
        .borrow_mut()
        .insert("sudo".to_owned());

    assert_eq!(setup_size(&fx, "20G"), 1);
    assert!(fx.root_commands().is_empty());
    assert_eq!(fx.image_size(), 10 * GIB, "nothing runs");
    let out = fx.stdout();
    assert!(out.contains("As root, run:"), "{out}");
    let truncate = out.find("truncate -c -s 20G").expect("truncate shown");
    let reload = out.find("losetup -c /dev/loop0").expect("losetup shown");
    let resize = out
        .find("btrfs filesystem resize 20G")
        .expect("resize shown");
    assert!(truncate < reload && reload < resize, "{out}");
}

#[test]
fn refuses_a_volume_that_is_not_its_image() {
    // A btrfs of the user's own: no loop device backed by ramet's image.
    let fx = Fixture::new();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(("/dev/sdb1".to_owned(), String::new()));
    machine.borrow_mut().mounted = true;

    assert_eq!(setup_size(&fx, "20G"), 1);
    assert!(fx.root_commands().is_empty());
    let err = fx.stderr();
    assert!(err.contains("not from ramet's image"), "{err}");
    assert!(
        err.contains("a btrfs of your own is yours to resize"),
        "{err}"
    );
}

#[test]
fn creates_the_image_at_the_size_asked() {
    let fx = Fixture::new();
    std::fs::remove_dir(&fx.root).unwrap();
    fx.machine();
    let image = fx.layout().data_image().to_owned();
    fx.host.0.fs_size.set(30 * GIB);
    fx.runner.handle(move |cmd| {
        let argv = cmd.argv();
        match argv[0].as_str() {
            "losetup" => Some(ramet::process::Output::success_with(format!(
                "{}\n",
                image.display()
            ))),
            "lsblk" => Some(ramet::process::Output::success_with(format!(
                "{}\n",
                30 * GIB
            ))),
            _ => None,
        }
    });

    assert_eq!(setup_size(&fx, "30G"), 0, "{}{}", fx.stdout(), fx.stderr());
    assert_eq!(fx.image_size(), 30 * GIB);
    let out = fx.stdout();
    assert!(out.contains("created: 30.0 GiB"), "{out}");
    assert!(!out.contains("resize"), "created at its size: {out}");
}
