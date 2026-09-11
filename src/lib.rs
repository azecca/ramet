//! ramet: one git branch, one docker compose stack, data of its own.
//!
//! An *env* is a git worktree, a btrfs subvolume holding the named volumes of
//! the project's compose stack, and an isolated compose project. Creating an
//! env clones the current one (a writable btrfs snapshot plus
//! `git worktree add`); a *checkpoint* is a named read-only snapshot the env
//! can be rewound to. The project's own files are never modified.
//!
//! This library backs the `ramet` binary; its API is not meant to be stable.
//!
//! Layout of the crate:
//!
//! - [`cli`] parses the command line and [`app`] runs the parsed command.
//! - [`commands`] holds one module per command.
//! - [`env`](mod@env) is the domain: env metadata, discovery, ports, synced files.
//! - [`settings`] reads the project's optional `.ramet.json`.
//! - [`compose`], [`git`], [`docker`] and [`storage`] drive the external
//!   tools, all through the single [`process::Runner`] gateway.
//! - [`context`] gathers what a command needs from the outside world, so that
//!   tests can substitute every external dependency.

pub mod app;
pub mod cli;
pub mod commands;
pub mod compose;
pub mod context;
pub mod docker;
pub mod env;
pub mod error;
pub mod git;
pub mod host;
pub mod interrupt;
pub mod layout;
pub mod ports;
pub mod process;
pub mod settings;
pub mod storage;
pub mod ui;
pub mod util;
