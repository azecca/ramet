//! `ramet setup`: every step skipped when done, root steps shown then run
//! through sudo, and never an existing fstab line touched.

use std::fs;

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;
use ramet::layout::DATA_IMAGE_SIZE;

use crate::support::{Fixture, make_read_only};

const GIB: u64 = 1 << 30;

/// Runs `ramet setup` and returns its exit code.
fn setup(fx: &Fixture) -> u8 {
    let cli = Cli::try_parse_from(["ramet", "setup"]).unwrap();
    app::run(&fx.ctx(), &cli.command)
}

/// A machine where nothing is prepared yet: no mount point, no fstab line, no image.
fn bare_machine(fx: &Fixture) -> std::rc::Rc<std::cell::RefCell<crate::support::Machine>> {
    fs::remove_dir(&fx.root).unwrap();
    fx.machine()
}

fn ran(fx: &Fixture, program: &str) -> bool {
    fx.runner
        .argvs()
        .iter()
        .any(|argv| argv[0].ends_with(program))
}

#[test]
fn prepares_a_bare_machine_in_one_go() {
    let fx = Fixture::new();
    let machine = bare_machine(&fx);

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());

    let image = fx.layout().data_image().to_owned();
    assert_eq!(fs::metadata(&image).unwrap().len(), DATA_IMAGE_SIZE);
    let mkfs = fx
        .runner
        .argvs()
        .into_iter()
        .find(|argv| argv[0].ends_with("mkfs.btrfs"))
        .unwrap();
    assert!(
        mkfs.contains(&"--rootdir".to_owned()),
        "the volume's root must belong to the user: {mkfs:?}"
    );
    assert_eq!(mkfs.last().unwrap(), &image.display().to_string());
    assert!(
        !fx.base.join(".ramet-empty-root").exists(),
        "the empty directory is removed"
    );

    let root = fx.root.display().to_string();
    assert_eq!(
        fx.root_commands(),
        [
            vec!["sudo".to_owned(), "mkdir".into(), "-p".into(), root],
            vec![
                "sudo".into(),
                "tee".into(),
                "-a".into(),
                "/etc/fstab".into()
            ],
        ]
    );
    assert!(
        machine
            .borrow()
            .appended
            .ends_with(&fx.layout().fstab_line()),
        "{}",
        machine.borrow().appended
    );
    assert!(machine.borrow().mounted);

    let out = fx.stdout();
    assert!(out.contains("ramet is ready"), "{out}");
    assert!(out.contains("ramet init"), "{out}");
    assert!(
        out.find("mkdir -p").unwrap() < out.find("line added").unwrap(),
        "root steps are shown before they run: {out}"
    );
}

#[test]
fn a_ready_machine_is_left_alone() {
    let fx = Fixture::new();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());
    machine.borrow_mut().mounted = true;

    assert_eq!(setup(&fx), 0);
    assert!(fx.root_commands().is_empty());
    assert!(!ran(&fx, "mkfs.btrfs") && !ran(&fx, "mount"));
    assert!(!fx.layout().data_image().exists());
    assert!(fx.stdout().contains("ramet is ready"));
}

#[test]
fn completes_a_half_prepared_machine_without_touching_fstab() {
    // The fstab line is there, but the image and the mount point are gone.
    let fx = Fixture::new();
    let machine = bare_machine(&fx);
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    assert!(fx.layout().data_image().exists());
    assert_eq!(
        fx.root_commands(),
        [vec![
            "sudo".to_owned(),
            "mkdir".into(),
            "-p".into(),
            fx.root.display().to_string()
        ]]
    );
    assert!(machine.borrow().appended.is_empty());
    assert!(fx.stdout().contains("line present"));
}

#[test]
fn an_existing_image_is_reused() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some(fx.image_fstab_line());
    fs::write(fx.layout().data_image(), "data").unwrap();

    assert_eq!(setup(&fx), 0);
    assert!(!ran(&fx, "mkfs.btrfs"));
    assert_eq!(fs::read(fx.layout().data_image()).unwrap(), b"data");
}

#[test]
fn a_line_mounting_something_else_is_used_as_it_is() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some((
        "UUID=1234-abcd".to_owned(),
        ramet::layout::FSTAB_OPTIONS.to_owned(),
    ));

    assert_eq!(setup(&fx), 0);
    assert!(!fx.layout().data_image().exists(), "no image is needed");
    assert!(fx.root_commands().is_empty());
    assert!(fx.stdout().contains("mounts UUID=1234-abcd"));
}

#[test]
fn an_incomplete_line_is_refused_not_edited() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some((
        fx.layout().data_image().display().to_string(),
        "noauto,loop".to_owned(),
    ));

    assert_eq!(setup(&fx), 1);
    let err = fx.stderr();
    assert!(
        err.contains("lacks `user` and `user_subvol_rm_allowed`"),
        "{err}"
    );
    assert!(err.contains("never edits"), "{err}");
    assert!(fx.root_commands().is_empty());
}

#[test]
fn a_line_mounting_a_vanished_file_is_refused() {
    let fx = Fixture::new();
    fx.machine().borrow_mut().fstab = Some((
        fx.base.join("elsewhere.img").display().to_string(),
        ramet::layout::FSTAB_OPTIONS.to_owned(),
    ));

    assert_eq!(setup(&fx), 1);
    assert!(
        fx.stderr().contains("which does not exist"),
        "{}",
        fx.stderr()
    );
    assert!(!fx.layout().data_image().exists());
}

#[test]
fn without_sudo_the_root_steps_are_shown_for_the_user_to_run() {
    let fx = Fixture::new();
    let machine = bare_machine(&fx);
    fx.host
        .0
        .missing_programs
        .borrow_mut()
        .insert("sudo".to_owned());

    assert_eq!(setup(&fx), 1);
    assert!(fx.root_commands().is_empty());
    assert!(!machine.borrow().mounted);
    assert!(
        fx.layout().data_image().exists(),
        "what needs no root is done"
    );
    let out = fx.stdout();
    assert!(out.contains("sudo is not installed"), "{out}");
    assert!(
        out.contains(&format!("mkdir -p {}", fx.root.display())),
        "{out}"
    );
    assert!(out.contains("cat >> /etc/fstab <<'EOF'"), "{out}");
    assert!(out.contains(fx.layout().fstab_line().trim_end()), "{out}");
    assert!(out.contains("run the same `ramet setup` again"), "{out}");
}

#[test]
fn a_root_environment_runs_the_steps_itself() {
    let fx = Fixture::new();
    bare_machine(&fx);
    fx.host.0.euid.set(0);

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    let programs: Vec<String> = fx
        .root_commands()
        .into_iter()
        .map(|argv| argv[0].clone())
        .collect();
    assert_eq!(programs, ["mkdir", "tee"]);
}

#[test]
fn refuses_to_run_under_sudo() {
    // The image would belong to root, and the user could not use it.
    let fx = Fixture::new();
    fx.machine();
    fx.host.0.euid.set(0);
    fx.host
        .0
        .vars
        .borrow_mut()
        .insert("SUDO_USER".into(), "alex".into());

    assert_eq!(setup(&fx), 1);
    assert!(fx.stderr().contains("sudo ramet setup"), "{}", fx.stderr());
    assert!(fx.runner.external_argvs().is_empty());
    assert!(!fx.layout().data_image().exists());
}

#[test]
fn a_volume_root_owned_by_someone_else_is_handed_over() {
    let fx = Fixture::new();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(fx.image_fstab_line());
    fs::write(fx.layout().data_image(), "").unwrap();
    if !make_read_only(&fx.root) {
        return;
    }

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    assert_eq!(
        fx.root_commands(),
        [vec![
            "sudo".to_owned(),
            "chown".into(),
            "tester".into(),
            fx.root.display().to_string()
        ]]
    );
    assert!(fx.stdout().contains("handed over to tester"));
}

#[test]
fn a_failed_sudo_stops_everything() {
    let fx = Fixture::new();
    let machine = bare_machine(&fx);
    machine.borrow_mut().sudo_fails = true;

    assert_eq!(setup(&fx), 1);
    assert!(
        fx.stderr().contains("3 incorrect password attempts"),
        "{}",
        fx.stderr()
    );
    assert!(!ran(&fx, "mount"));
    assert_eq!(fx.root_commands().len(), 1, "nothing after the failure");
}

#[test]
fn stops_early_without_btrfs_progs() {
    let fx = Fixture::new();
    bare_machine(&fx);
    fx.host
        .0
        .missing_programs
        .borrow_mut()
        .insert("btrfs".to_owned());

    assert_eq!(setup(&fx), 1);
    assert!(
        fx.stdout().contains("install btrfs-progs"),
        "{}",
        fx.stdout()
    );
    assert!(!fx.layout().data_image().exists());
    assert!(fx.root_commands().is_empty());
}

#[test]
fn prepares_the_volume_even_when_docker_is_missing() {
    // Docker is needed by envs, not by the volume: the user installs it
    // afterwards, without running setup again.
    let fx = Fixture::new();
    let machine = bare_machine(&fx);
    fx.host
        .0
        .missing_programs
        .borrow_mut()
        .insert("docker".to_owned());

    assert_eq!(setup(&fx), 1);
    assert!(machine.borrow().mounted);
    let out = fx.stdout();
    assert!(out.contains("install Docker Engine"), "{out}");
    assert!(out.contains("The data volume is ready"), "{out}");
    assert!(!out.contains("ramet is ready"), "{out}");
}

#[test]
fn a_foreign_filesystem_on_the_mount_point_is_refused() {
    let fx = Fixture::new();
    // Handlers answer in order: this one shadows the machine's findmnt.
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        (argv[0] == "findmnt" && argv.contains(&"--mountpoint".to_owned()))
            .then(|| ramet::process::Output::success_with("ext4\n"))
    });
    fx.machine();

    assert_eq!(setup(&fx), 1);
    assert!(fx.stderr().contains("not btrfs"), "{}", fx.stderr());
    assert!(fx.root_commands().is_empty());
}

/// The programs root ran through sudo, in order.
fn root_lines(fx: &Fixture) -> Vec<String> {
    fx.root_commands()
        .into_iter()
        .map(|argv| argv[1..].join(" "))
        .collect()
}

#[test]
fn turns_btrfs_quotas_on_for_its_image() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, GIB);
    let sysfs = fx.quotas_off();
    let root = fx.root.display();

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    assert_eq!(
        root_lines(&fx),
        [
            format!("btrfs quota enable {root}"),
            format!("btrfs quota rescan -w {root}"),
        ]
    );
    assert!(sysfs.join("fs/btrfs/0000-test/qgroups").is_dir());
    let out = fx.stdout();
    assert!(out.contains("exact sizes for `ramet df`"), "{out}");

    // Once on, a second setup leaves them alone.
    let again = Fixture::new();
    again.image_volume(10 * GIB, 10 * GIB, 10 * GIB, GIB);
    again.quotas(&[]);
    assert_eq!(setup(&again), 0);
    assert!(again.root_commands().is_empty());
    assert!(
        again
            .stdout()
            .contains("on: `ramet df` measures every env exactly"),
        "{}",
        again.stdout()
    );
}

#[test]
fn has_inconsistent_quota_counts_redone() {
    let fx = Fixture::new();
    fx.image_volume(10 * GIB, 10 * GIB, 10 * GIB, GIB);
    let qgroups = fx.quotas(&[]);
    fs::write(qgroups.join("inconsistent"), "1\n").unwrap();

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    assert_eq!(
        root_lines(&fx),
        [format!("btrfs quota rescan -w {}", fx.root.display())]
    );
}

#[test]
fn leaves_quotas_of_a_btrfs_of_its_own_to_the_user() {
    // No loop device backed by ramet's image: quotas would count the user's
    // whole filesystem.
    let fx = Fixture::new();
    let machine = fx.machine();
    machine.borrow_mut().fstab = Some(("/dev/sdb1".to_owned(), String::new()));
    machine.borrow_mut().mounted = true;
    fx.quotas_off();

    assert_eq!(setup(&fx), 0, "{}{}", fx.stdout(), fx.stderr());
    assert!(fx.root_commands().is_empty());
}
