//! `ramet doctor` on the data volume: green only when the next command can
//! really mount it.

use std::fs;

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;

use crate::support::Fixture;

/// Runs `ramet doctor` and returns its exit code.
fn doctor(fx: &Fixture) -> u8 {
    let cli = Cli::try_parse_from(["ramet", "doctor"]).unwrap();
    app::run(&fx.ctx(), &cli.command)
}

#[test]
fn a_complete_fstab_line_is_enough_to_be_green() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some(fx.image_fstab_line());
    fs::write(fx.layout().data_image(), "").unwrap();

    assert_eq!(doctor(&fx), 0, "{}", fx.stdout());
    let out = fx.stdout();
    assert!(out.contains("/etc/fstab line present"), "{out}");
    assert!(out.contains("Everything is in order"), "{out}");
}

#[test]
fn a_line_whose_image_and_mount_point_are_gone_is_not_green() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some(fx.image_fstab_line());
    fs::remove_dir(&fx.root).unwrap();

    assert_eq!(doctor(&fx), 1);
    let out = fx.stdout();
    assert!(
        out.contains(&format!(
            "the mount point {} does not exist",
            fx.root.display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "mounts {}, which does not exist",
            fx.layout().data_image().display()
        )),
        "{out}"
    );
    assert!(out.contains("run `ramet setup`"), "{out}");
    assert!(!out.contains("/etc/fstab line present"), "{out}");
    assert!(!out.contains("Everything is in order"), "{out}");
}

#[test]
fn a_missing_fstab_line_points_to_setup() {
    let fx = Fixture::new();
    fx.machine();

    assert_eq!(doctor(&fx), 1);
    let out = fx.stdout();
    assert!(out.contains("no /etc/fstab line describes it"), "{out}");
    assert!(out.contains("run `ramet setup`"), "{out}");
}

#[test]
fn an_incomplete_fstab_line_is_reported() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some((
        fx.layout().data_image().display().to_string(),
        "noauto,loop".to_owned(),
    ));
    fs::write(fx.layout().data_image(), "").unwrap();

    assert_eq!(doctor(&fx), 1);
    let out = fx.stdout();
    assert!(out.contains("lacks `user`"), "{out}");
    assert!(out.contains("lacks `user_subvol_rm_allowed`"), "{out}");
}
