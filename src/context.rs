//! Everything a command needs from the outside world, in one place.
//!
//! Commands never reach for the process environment directly: the working
//! directory, the terminal, external programs, btrfs and host queries all come
//! from a [`Context`]. Production code builds it with [`Context::system`];
//! tests build it with fakes.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::compose::Compose;
use crate::docker::Docker;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::host::{Host, SystemHost};
use crate::layout::Layout;
use crate::process::{Runner, SystemRunner};
use crate::storage::{Btrfs, BtrfsCli, DataVolume, Sizes, Subvolumes};
use crate::ui::Ui;

/// The adapters to the outside world a [`Context`] is built from.
pub struct Services {
    /// Runs external programs.
    pub runner: Rc<dyn Runner>,
    /// Manipulates btrfs subvolumes.
    pub btrfs: Box<dyn Btrfs>,
    /// Answers questions about the host.
    pub host: Box<dyn Host>,
}

/// The environment a command runs in.
pub struct Context {
    layout: Layout,
    cwd: PathBuf,
    ui: Ui,
    runner: Rc<dyn Runner>,
    btrfs: Box<dyn Btrfs>,
    host: Box<dyn Host>,
}

impl Context {
    /// A context from explicit parts.
    pub fn new(layout: Layout, cwd: PathBuf, ui: Ui, services: Services) -> Self {
        Self {
            layout,
            cwd,
            ui,
            runner: services.runner,
            btrfs: services.btrfs,
            host: services.host,
        }
    }

    /// The real system: standard streams, current directory, `/srv/ramet`.
    pub fn system(verbose: bool) -> Result<Self> {
        let ui = Ui::stdio();
        let cwd = std::env::current_dir().map_err(|source| Error::io(Path::new("."), source))?;
        let runner: Rc<dyn Runner> = Rc::new(SystemRunner::new(verbose, ui.style()));
        let services = Services {
            btrfs: Box::new(BtrfsCli::new(Rc::clone(&runner))),
            runner,
            host: Box::new(SystemHost),
        };
        Ok(Self::new(Layout::standard(), cwd, ui, services))
    }

    /// Where ramet keeps its data.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The directory the command was started from; it selects the current env.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The user's terminal.
    pub fn ui(&self) -> &Ui {
        &self.ui
    }

    /// The gateway to external programs.
    pub fn runner(&self) -> &dyn Runner {
        self.runner.as_ref()
    }

    /// Facts about the host.
    pub fn host(&self) -> &dyn Host {
        self.host.as_ref()
    }

    /// Subvolume operations, deletions guarded.
    pub fn subvolumes(&self) -> Subvolumes<'_> {
        Subvolumes::new(self.btrfs.as_ref(), self.layout.root())
    }

    /// Git operations.
    pub fn git(&self) -> Git<'_> {
        Git::new(self.runner())
    }

    /// Docker volume operations.
    pub fn docker(&self) -> Docker<'_> {
        Docker::new(self.runner())
    }

    /// Docker compose operations.
    pub fn compose(&self) -> Compose<'_> {
        Compose::new(self.runner(), self.host())
    }

    /// Low-level btrfs operations, unguarded: prefer [`subvolumes`](Self::subvolumes).
    pub fn btrfs(&self) -> &dyn Btrfs {
        self.btrfs.as_ref()
    }

    /// Measures subvolumes: exactly when btrfs quotas are on.
    pub fn sizes(&self) -> Sizes<'_> {
        Sizes::new(self.btrfs.as_ref(), &self.data_volume().quotas())
    }

    /// The btrfs data volume.
    pub fn data_volume(&self) -> DataVolume<'_> {
        DataVolume::new(&self.layout, self.runner(), self.host())
    }
}
