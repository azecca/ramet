//! The `ramet` command.

use std::process::ExitCode;

use clap::Parser;

use ramet::app;
use ramet::cli::{Cli, Command};
use ramet::context::Context;
use ramet::ui::Ui;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ctx = match Context::system(cli.verbose) {
        Ok(ctx) => ctx,
        // The working directory is gone, typically a shell left in a worktree
        // removed since. That is outside any env, and a prompt never fails.
        Err(_) if matches!(cli.command, Command::Prompt(_)) => return ExitCode::SUCCESS,
        Err(err) => {
            app::report_error(&Ui::stdio(), &err);
            return ExitCode::FAILURE;
        }
    };
    ExitCode::from(app::run(&ctx, &cli.command))
}
