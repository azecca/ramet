//! `ramet doctor` on the data volume: green only when the next command can
//! really mount it.

use std::fs;

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;
use serde_json::json;

use crate::support::{Fixture, write_settings};

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

#[test]
fn a_restore_cut_short_mid_swap_is_reported_with_the_command_that_repairs_it() {
    // Killed between the two renames: feat-a's data sits under its outgoing
    // name, and its own place is empty.
    let fx = Fixture::with_main_env();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());
    machine.borrow_mut().mounted = true;
    fs::write(fx.layout().data_image(), "").unwrap();
    fx.secondary_env("feat-a");
    fs::rename(fx.env_dir("feat-a"), fx.env_dir("feat-a@.replaced")).unwrap();
    fs::create_dir_all(fx.env_dir("main@.restoring")).unwrap();

    doctor(&fx);
    let out = fx.stdout();
    assert!(
        out.contains(&format!(
            "`mv {} {}` puts it back",
            fx.env_dir("feat-a@.replaced").display(),
            fx.env_dir("feat-a").display()
        )),
        "{out}"
    );
    assert!(out.contains("the next restore deletes it"), "{out}");
    assert!(
        !out.contains("missing from env.json: main@.restoring"),
        "{out}"
    );
}

#[test]
fn a_named_port_that_is_not_published_is_reported() {
    let fx = Fixture::with_main_env();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());
    machine.borrow_mut().mounted = true;
    fs::write(fx.layout().data_image(), "").unwrap();
    write_settings(&fx.clone, &json!({"ports": {"web": "proxy:80"}}));

    doctor(&fx);
    let out = fx.stdout();
    assert!(
        out.contains("ports.web: proxy:80 is not published, RAMET_PORT_WEB is not set"),
        "{out}"
    );
}

#[test]
fn a_stack_left_paused_is_reported() {
    let fx = Fixture::with_main_env();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());
    machine.borrow_mut().mounted = true;
    fs::write(fx.layout().data_image(), "").unwrap();
    fx.runner.set_containers(
        r#"[{"Service":"db","State":"paused"},{"Service":"web","State":"running"}]"#,
    );

    assert_eq!(doctor(&fx), 1);
    let out = fx.stdout();
    assert!(
        out.contains("paused, left frozen by a command that did not finish: db"),
        "{out}"
    );
    assert!(out.contains("`ramet compose unpause`"), "{out}");
}
