//! Ctrl-C while a stack is frozen.
//!
//! A test binary of its own: the signal handler and what it records are
//! process-wide, and must not leak into the other tests.

#[path = "integration/support/mod.rs"]
mod support;

use clap::Parser;
use ramet::app;
use ramet::cli::Cli;
use ramet::process::Output;
use signal_hook::consts::SIGINT;
use signal_hook::low_level::raise;

use support::{Fixture, PROJECT};

#[test]
fn ctrl_c_during_a_freeze_thaws_the_stack_and_stops_the_command() {
    let fx = Fixture::with_main_env();
    std::fs::write(
        fx.layout().compose_file(PROJECT, "main"),
        r#"{"services": {"web": {}}}"#,
    )
    .unwrap();
    fx.runner
        .set_containers(r#"[{"Service":"web","State":"running"}]"#);
    fx.runner.handle(|cmd| {
        let argv = cmd.argv();
        if argv[0] == "findmnt" && argv.contains(&"--target".to_owned()) {
            return Some(Output::success_with("btrfs\n"));
        }
        // Ctrl-C lands while the stack is being frozen. Without the deferral
        // the signal would end the test process here.
        if argv.last().is_some_and(|verb| verb == "pause") {
            raise(SIGINT).expect("raise SIGINT");
        }
        None
    });

    let cli = Cli::try_parse_from(["ramet", "checkpoint", "c1"]).unwrap();
    let code = app::run(&fx.ctx(), &cli.command);

    assert_eq!(code, 130);
    assert!(fx.stderr().contains("interrupted"), "{}", fx.stderr());
    let verbs = fx.runner.verbs_for("main");
    assert_eq!(
        verbs.last().map(String::as_str),
        Some("unpause"),
        "{verbs:?}"
    );
    assert!(
        fx.load("main").checkpoints.is_empty(),
        "an interrupted checkpoint is not recorded"
    );
}
