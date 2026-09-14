//! Integration tests: commands exercised against a temporary data root, real
//! git repositories, and simulated docker and btrfs.
//!
//! What needs a real subvolume or a real container (the positive case of the
//! inode check, the migration of docker volumes, the actual isolation of data
//! between envs) is covered by the reference scenario, `tests/scenario.sh` at
//! the root of the repository, run against the binary built here.

mod support;

mod checkpoint;
mod data_volume;
mod deinit;
mod df;
mod discovery;
mod dispatch;
mod doctor;
mod init;
mod listing;
mod locking;
mod new;
mod ports;
mod prune;
mod regenerate;
mod resize;
mod restore;
mod rm;
mod setup;
mod sync;
