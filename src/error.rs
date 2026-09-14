//! Errors reported to the user.
//!
//! Every failure ramet anticipates is a variant here, so that its wording
//! lives in one place and tests can match on the cause rather than on text.
//! Each error has a one-line message ([`std::fmt::Display`]) and, when there
//! is something the user can do about it, a [`hint`](Error::hint).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::FILE_NAME;

/// Result type of every fallible operation in ramet.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A failure reported to the user, with a non-zero exit code.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ---------------------------------------------------------------- system
    /// An external program exited with a non-zero code.
    #[error("`{command}` failed: {detail}")]
    CommandFailed {
        /// The command line, as it could be pasted into a shell.
        command: String,
        /// Its error output, or its exit code when it printed nothing.
        detail: String,
    },

    /// Ctrl-C was pressed while the stack of an env was frozen; the stack
    /// has been thawed since.
    #[error("interrupted")]
    Interrupted,

    /// An external program could not be found.
    #[error("command not found: {program}")]
    CommandNotFound {
        /// The missing program.
        program: String,
    },

    /// A filesystem operation failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory concerned.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },

    /// A question needs an answer but there is no terminal to ask it on.
    #[error("{question}")]
    ConfirmationRequired {
        /// The question that could not be asked.
        question: String,
    },

    /// The command is neither ramet's nor, after `ramet compose`, a known
    /// docker compose one.
    #[error("unknown command \"{word}\"")]
    UnknownCommand {
        /// What the user typed.
        word: String,
        /// The known command line it resembles, `ramet …` or `ramet compose …`.
        suggestion: Option<String>,
    },

    /// A docker compose command typed without `ramet compose`.
    #[error("\"{word}\" is a docker compose command")]
    BareComposeCommand {
        /// The compose command.
        word: String,
        /// The whole command line to type instead.
        command: String,
    },

    /// One of ramet's own commands typed after `ramet compose`.
    #[error("\"{word}\" is a ramet command, not a docker compose one")]
    RametCommandInCompose {
        /// The ramet command.
        word: String,
        /// The whole command line to type instead.
        command: String,
        /// The compose command it also resembles, if any.
        compose: Option<String>,
    },

    /// ramet was started through sudo from a regular account.
    #[error("ramet does not need sudo, and `sudo ramet {command}` would break your environment")]
    SudoRefused {
        /// The ramet command that was attempted.
        command: String,
        /// The account sudo was invoked from.
        user: String,
    },

    // ----------------------------------------------------------- data volume
    /// Something other than btrfs is mounted on the data root.
    #[error("{} already holds a {filesystem} mount, not btrfs", root.display())]
    ForeignMount {
        /// The data root.
        root: PathBuf,
        /// The filesystem type found there.
        filesystem: String,
    },

    /// The data volume is not mounted and could not be mounted.
    #[error("{} is not mounted{}", root.display(), detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    NotMounted {
        /// The data root.
        root: PathBuf,
        /// Why `mount` failed, if it said so.
        detail: Option<String>,
    },

    /// The data volume is mounted but the user cannot write to it.
    #[error("{} is mounted but does not belong to you", root.display())]
    RootNotWritable {
        /// The data root.
        root: PathBuf,
    },

    /// The existing `/etc/fstab` line for the data volume lacks options
    /// ramet depends on.
    #[error("the /etc/fstab line for {} lacks {}", root.display(), missing.iter().map(|option| format!("`{option}`")).collect::<Vec<_>>().join(" and "))]
    FstabLineIncomplete {
        /// The data root.
        root: PathBuf,
        /// The missing options.
        missing: Vec<&'static str>,
    },

    /// The existing `/etc/fstab` line for the data volume mounts a file that
    /// does not exist.
    #[error("the /etc/fstab line for {} mounts {}, which does not exist", root.display(), source_file.display())]
    FstabSourceMissing {
        /// The data root.
        root: PathBuf,
        /// The file the line mounts.
        source_file: PathBuf,
    },

    /// The data root does not exist.
    #[error("{} does not exist", root.display())]
    RootMissing {
        /// The data root.
        root: PathBuf,
    },

    /// `setup --size` on a data volume that is not ramet's image.
    #[error("{} is mounted from {}, not from ramet's image", root.display(), if mounted_from.is_empty() { "elsewhere" } else { mounted_from })]
    VolumeNotResizable {
        /// The data root.
        root: PathBuf,
        /// What it is mounted from.
        mounted_from: String,
    },

    /// `setup --size` below what the data volume holds.
    #[error("{} is too small: the data volume holds {}", crate::util::size::human_bytes(Some(*requested)), crate::util::size::human_bytes(Some(*used)))]
    VolumeTooSmall {
        /// The size asked for.
        requested: u64,
        /// Bytes in use.
        used: u64,
        /// The smallest size accepted.
        minimum: u64,
    },

    /// btrfs still spans more than the image is about to keep.
    #[error("btrfs still spans {}, more than the {} the image would keep: the image was left as it is", crate::util::size::human_bytes(Some(*filesystem)), crate::util::size::human_bytes(Some(*requested)))]
    ShrinkIncomplete {
        /// Size of the btrfs filesystem.
        filesystem: u64,
        /// The size asked for.
        requested: u64,
    },

    /// A subvolume deletion was refused by the safety checks.
    #[error("refusing to delete {}: {reason}", path.display())]
    DeletionRefused {
        /// The path whose deletion was requested.
        path: PathBuf,
        /// Which check failed.
        reason: DeletionRefusal,
    },

    // ------------------------------------------------------------- discovery
    /// The working directory is not inside a git repository.
    #[error("{} is not inside a git repository", path.display())]
    NotInRepository {
        /// The directory that was inspected.
        path: PathBuf,
    },

    /// No env is recorded for the current worktree, nor for its repository.
    #[error("no ramet env for {}", worktree.display())]
    NoEnvironment {
        /// The current worktree.
        worktree: PathBuf,
    },

    /// The current worktree belongs to a ramet project but has no env.
    #[error("the worktree {} belongs to project \"{project}\" but has no env", worktree.display())]
    UnmanagedWorktree {
        /// The current worktree.
        worktree: PathBuf,
        /// The project its repository belongs to.
        project: String,
    },

    /// The current repository has no ramet project.
    #[error("no ramet env for this repository{}", worktree.as_deref().map(|w| format!(" ({})", w.display())).unwrap_or_default())]
    NoProject {
        /// The current worktree, if inside a repository.
        worktree: Option<PathBuf>,
    },

    /// The named env does not exist in the project.
    #[error("unknown env \"{name}\" in project \"{project}\"")]
    UnknownEnvironment {
        /// The requested name.
        name: String,
        /// The current project.
        project: String,
        /// The envs that do exist.
        known: Vec<String>,
    },

    // -------------------------------------------------------------- settings
    /// A `.ramet.json` cannot be used.
    #[error("{}: {detail}", path.display())]
    InvalidSettings {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        detail: String,
    },

    // --------------------------------------------------------------- compose
    /// A declared compose file lies outside the worktree.
    #[error("compose file outside the worktree: {file}")]
    ComposeFileOutsideWorktree {
        /// The file as given.
        file: String,
        /// The worktree it should be under.
        worktree: PathBuf,
    },

    /// A declared compose file does not exist.
    #[error("compose file not found: {}", path.display())]
    ComposeFileMissing {
        /// The missing file.
        path: PathBuf,
    },

    /// Docker compose would find no compose file in the worktree.
    #[error("no compose file at the root of {}", worktree.display())]
    ComposeNotDiscoverable {
        /// The worktree.
        worktree: PathBuf,
        /// A compose file in the subdirectory the command was run from,
        /// relative to the worktree.
        nested: Option<PathBuf>,
    },

    /// `docker compose config` failed.
    #[error("`docker compose config` failed in {}:\n{detail}", worktree.display())]
    ComposeConfigFailed {
        /// The worktree it ran in.
        worktree: PathBuf,
        /// The last lines of its error output.
        detail: String,
    },

    /// `docker compose config` printed something that is not a JSON object.
    #[error("unreadable output from `docker compose config`: {0}")]
    ComposeConfigUnreadable(#[source] serde_json::Error),

    /// `/etc/fstab` has a line for the data root that `findmnt` did not read.
    #[error("/etc/fstab has a line for {}, which findmnt does not read", root.display())]
    FstabLineUnread {
        /// The data root.
        root: PathBuf,
    },

    /// The data image to format already holds something.
    #[error("{} is not empty: ramet only formats the empty image it has root create", image.display())]
    ImageNotEmpty {
        /// The image.
        image: PathBuf,
    },

    /// A volume ramet would store has a name that is no plain directory name,
    /// or a docker name a command would read as an option.
    #[error(
        "the compose configuration names a volume {name:?}, which ramet cannot store \
         (letters, digits, '.', '_' and '-' only, not starting with '.', '_' or '-')"
    )]
    InvalidVolumeName {
        /// The name, as the configuration gives it.
        name: String,
    },

    // ----------------------------------------------------------------- names
    /// A name reduces to nothing once sanitized.
    #[error("unusable project name: {name:?}")]
    InvalidProjectName {
        /// The name as given.
        name: String,
    },

    /// An env name or checkpoint label contains forbidden characters.
    #[error(
        "invalid {kind} {name:?} (letters, digits, '.', '_' and '-' only, not starting with '.', '_' or '-')"
    )]
    InvalidName {
        /// What was being named: "env name" or "label".
        kind: &'static str,
        /// The name as given.
        name: String,
    },

    // ----------------------------------------------------------------- ports
    /// The project publishes more ports than the allocation range can hold.
    #[error("{count} published ports: too many for the range {first}-{last}")]
    TooManyPorts {
        /// Number of published ports.
        count: usize,
        /// First port of the allocation range.
        first: u16,
        /// Last port of the allocation range.
        last: u16,
    },

    /// No block of consecutive free ports is left.
    #[error("no block of {span} free ports between {first} and {last}")]
    NoFreePortRange {
        /// Size of the block that was searched for.
        span: usize,
        /// First port of the allocation range.
        first: u16,
        /// Last port of the allocation range.
        last: u16,
    },

    // ------------------------------------------------------------------ init
    /// `init` was run from a secondary worktree.
    #[error("`ramet init` must run in the main clone ({}), not in the worktree {}", main_clone.display(), worktree.display())]
    InitOutsideMainClone {
        /// The repository's main clone.
        main_clone: PathBuf,
        /// The worktree it was run from.
        worktree: PathBuf,
    },

    /// The project already has a ramet env.
    #[error("project \"{project}\" is already initialized: {}", path.display())]
    AlreadyInitialized {
        /// The project.
        project: String,
        /// Its data directory.
        path: PathBuf,
    },

    /// The main clone is not on the default branch, and there is no terminal
    /// to confirm the main env's name on.
    #[error("the main clone is on {current}, not on the default branch \"{branch}\"")]
    MainEnvNameRequired {
        /// Where the main clone is: `"feat-a"`, or a detached HEAD.
        current: String,
        /// The default branch.
        branch: String,
        /// The name the main env would take after it.
        name: String,
    },

    /// Volumes to migrate are missing under the chosen prefix.
    #[error("no volume under this prefix: {}", volumes.join(", "))]
    MigrationSourceMissing {
        /// The docker volumes that do not exist.
        volumes: Vec<String>,
    },

    /// Existing data may live under another prefix; the user must choose.
    #[error("{summary}")]
    MigrationChoiceRequired {
        /// What was found.
        summary: String,
        /// The commands that settle the choice.
        options: String,
    },

    /// The volumes to migrate do not fit in the data volume.
    #[error("{} of volumes to migrate, but only {} left in {}", crate::util::size::human_bytes(Some(*needed)), crate::util::size::human_bytes(Some(*available)), root.display())]
    InsufficientSpace {
        /// Bytes to copy.
        needed: u64,
        /// Bytes available.
        available: u64,
        /// The data root.
        root: PathBuf,
    },

    // ------------------------------------------------------------------ envs
    /// An env with this name already exists.
    #[error("env \"{name}\" already exists: {}", path.display())]
    EnvironmentExists {
        /// The name.
        name: String,
        /// Its subvolume.
        path: PathBuf,
    },

    /// The directory where a new worktree would go is already taken.
    #[error("{} already exists", path.display())]
    PathExists {
        /// The path.
        path: PathBuf,
    },

    /// The recorded worktree of the current env is not a git worktree.
    #[error("{} is not a git worktree", path.display())]
    NotAWorktree {
        /// The path.
        path: PathBuf,
    },

    /// The checkpoint does not exist.
    #[error("checkpoint \"{label}\" not found for env \"{env}\" ({})", path.display())]
    CheckpointNotFound {
        /// The label.
        label: String,
        /// The env.
        env: String,
        /// Where it was expected.
        path: PathBuf,
    },

    /// A checkpoint with this label already exists.
    #[error("checkpoint \"{label}\" already exists for env \"{env}\"")]
    CheckpointExists {
        /// The label.
        label: String,
        /// The env.
        env: String,
    },

    /// The repository has no commit, so a new worktree would be empty.
    #[error("the repository {} has no commit yet: a new worktree would be empty", worktree.display())]
    NoCommit {
        /// The worktree of the source env.
        worktree: PathBuf,
    },

    /// Compose files the new worktree needs are missing from the commit it
    /// would check out.
    #[error("{}", uncommitted_files(files, branch.as_deref()))]
    ComposeFilesNotCommitted {
        /// The missing files, relative to the worktree.
        files: Vec<String>,
        /// The existing branch the worktree would check out, if not a new
        /// branch started from the source's HEAD.
        branch: Option<String>,
    },

    /// A synced file would land outside the worktree it is synced into.
    #[error("synced path lies outside the worktree: {path}")]
    SyncedPathOutsideWorktree {
        /// The path, relative to the worktree.
        path: String,
    },

    /// `sync` has no env to take the files from.
    #[error("\"{env}\" has no parent env to sync from")]
    NoSyncSource {
        /// The current env.
        env: String,
        /// The other envs of the project.
        known: Vec<String>,
    },

    /// `sync --from` names the current env.
    #[error("\"{env}\" is the current env: there is nothing to sync from it")]
    SyncFromItself {
        /// The current env.
        env: String,
    },

    /// `rm` was asked to remove the env of the main clone.
    #[error("\"{name}\" cannot be removed: it is the env of the main clone")]
    PrimaryEnvironment {
        /// The env.
        name: String,
    },

    /// `rm` was asked to remove the env the user is standing in.
    #[error("\"{name}\" is the current env")]
    CurrentEnvironment {
        /// The env.
        name: String,
        /// Somewhere to go instead.
        main_clone: PathBuf,
    },

    /// `deinit` was run from a secondary env.
    #[error("`deinit` runs from the main clone, not from \"{env}\"")]
    DeinitOutsideMainClone {
        /// The current env.
        env: String,
        /// The main clone.
        main_clone: PathBuf,
    },

    /// `deinit` was run while secondary envs remain.
    #[error("{} env(s) remain: {}", names.len(), names.join(", "))]
    EnvironmentsRemain {
        /// The remaining secondary envs.
        names: Vec<String>,
    },

    /// The project directory still holds something unexpected.
    #[error("{} could not be removed: {source}", path.display())]
    ProjectDirNotRemovable {
        /// The project directory.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// Why a subvolume deletion was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionRefusal {
    /// The target is not under the data root.
    OutsideRoot {
        /// The data root.
        root: PathBuf,
    },
    /// The target is not exactly `<root>/<project>/<env>`.
    WrongDepth {
        /// Number of components below the root.
        depth: usize,
    },
    /// The target belongs to another project than the one the command acts in.
    OtherProject {
        /// The directory of the project the command acts in.
        project_dir: PathBuf,
    },
    /// The target is a plain directory, not a btrfs subvolume.
    NotSubvolume,
}

impl fmt::Display for DeletionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideRoot { root } => write!(f, "outside of {}", root.display()),
            Self::WrongDepth { depth } => {
                write!(f, "expected <root>/<project>/<env>, found depth {depth}")
            }
            Self::OtherProject { project_dir } => {
                write!(f, "not an env of {}", project_dir.display())
            }
            Self::NotSubvolume => f.write_str("not a btrfs subvolume"),
        }
    }
}

/// "compose.yml is not committed: the new worktree would not have it", and its
/// variants for several files or an existing branch.
fn uncommitted_files(files: &[String], branch: Option<&str>) -> String {
    let (verb, pronoun) = if files.len() == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    let place = branch.map_or_else(
        || "not committed".to_owned(),
        |branch| format!("not in branch \"{branch}\""),
    );
    format!(
        "{} {verb} {place}: the new worktree would not have {pronoun}",
        files.join(", ")
    )
}

/// What to do when compose would find no file at the root of `worktree`,
/// knowing the `nested` compose file of the directory the command ran from.
fn discovery_hint(worktree: &Path, nested: Option<&Path>) -> String {
    let Some(file) = nested else {
        return format!(
            "docker compose would climb to the parent directories and pick up an unrelated \
             project. Declare the project's files in {FILE_NAME}, at the root of the \
             repository:\n  {}",
            compose_files_example(&["docker/compose/base.yml", "docker/compose/dev.yml"])
        );
    };
    let dir = file.parent().unwrap_or(Path::new("."));
    format!(
        "{file} is in a subdirectory: ramet works on whole git repositories, and this one is \
         {worktree}.\nTo manage {dir}/ on its own, make it a repository of its own: `git init` \
         there, then commit its files.\nTo manage the whole repository, declare the file in \
         {FILE_NAME}, at its root: {example}",
        example = compose_files_example(&[&file.display().to_string()]),
        file = file.display(),
        worktree = worktree.display(),
        dir = dir.display()
    )
}

/// `names` joined with commas, or `(none)`.
fn listed(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_owned()
    } else {
        names.join(", ")
    }
}

/// A `.ramet.json` declaring `files` as the project's compose files.
fn compose_files_example(files: &[&str]) -> String {
    serde_json::json!({ "compose": { "files": files } }).to_string()
}

/// What a `.ramet.json` looks like, for the hint of an invalid one.
const SETTINGS_EXAMPLE: &str = r#"{"compose": {"files": ["compose.yml"], "profiles": ["dev"]}, "ports": {"web": "proxy:80"}, "sync": ["**/.env"]}"#;

/// What to do when the commit a new worktree checks out lacks compose files.
fn uncommitted_hint(files: &[String], branch: Option<&str>) -> String {
    match branch {
        None => format!(
            "a worktree only holds what is committed: commit first (`git add {} && git \
             commit`), then run `ramet new` again",
            files.join(" ")
        ),
        Some(branch) => format!(
            "the worktree checks out the existing branch \"{branch}\": commit the files there, \
             or choose another branch with --branch"
        ),
    }
}

impl Error {
    /// Wraps an I/O error with the path it concerns.
    pub fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// What the user can do about the error, if anything.
    pub fn hint(&self) -> Option<String> {
        if let Some(hint) = self.volume_hint() {
            return Some(hint);
        }
        let hint = match self {
            Self::ConfirmationRequired { .. } => {
                "there is no terminal to confirm on: rerun with --yes".to_owned()
            }
            Self::UnknownCommand {
                suggestion: Some(suggestion),
                ..
            } => format!("did you mean `{suggestion}`?"),
            Self::UnknownCommand { .. } => "`ramet --help` lists ramet's commands; docker compose \
                                            commands go through `ramet compose`"
                .to_owned(),
            Self::BareComposeCommand { command, .. } => {
                format!("docker compose commands go through `ramet compose`: `{command}`")
            }
            Self::RametCommandInCompose {
                command, compose, ..
            } => match compose {
                Some(compose) => format!("run `{command}`, or `{compose}` for docker compose"),
                None => format!("run it without `compose`: `{command}`"),
            },
            Self::SudoRefused { user, .. } => format!(
                "subvolumes and env.json files would belong to root, and {user} could no longer \
                 write to them. Rerun the command without sudo."
            ),
            Self::FstabLineUnread { .. } => "check it with `findmnt --verify`, and unset \
                                              LIBMOUNT_FSTAB if it is set: ramet never adds a second line"
                .to_owned(),
            Self::ImageNotEmpty { image } => format!(
                "if it holds nothing you need, remove it (`sudo rm {}`) and run `ramet setup` again",
                image.display()
            ),
            Self::InvalidVolumeName { .. } => "each volume gets a directory of its name in the \
                                                env's data: rename it in the compose file"
                .to_owned(),
            Self::NoEnvironment { .. } | Self::NoProject { .. } => {
                "run `ramet init` in the project's main clone".to_owned()
            }
            Self::UnmanagedWorktree { .. } => {
                "it was created outside ramet: use `ramet new` to get a complete env".to_owned()
            }
            Self::UnknownEnvironment { known, .. } => format!("known envs: {}", listed(known)),
            Self::ComposeFileOutsideWorktree { worktree, .. } => format!(
                "it must be under {}, otherwise it would not follow the envs created by `ramet new`",
                worktree.display()
            ),
            Self::ComposeNotDiscoverable { worktree, nested } => {
                discovery_hint(worktree, nested.as_deref())
            }
            Self::InvalidSettings { .. } => format!(
                "every key is optional; a complete {FILE_NAME} looks like:\n  {SETTINGS_EXAMPLE}"
            ),
            Self::NoSyncSource { known, .. } => format!(
                "choose the env to take the files from: `ramet sync --from {}` (other envs: {})",
                known.first().map_or("<env>", String::as_str),
                listed(known)
            ),
            Self::SyncFromItself { .. } => {
                "`--from` names another env of the project (`ramet ls` lists them)".to_owned()
            }
            Self::MigrationSourceMissing { .. } => {
                "check the name with `docker volume ls`".to_owned()
            }
            Self::MigrationChoiceRequired { options, .. } => options.clone(),
            Self::InsufficientSpace { .. } => "grow the data volume with `ramet setup --size \
                                               <size>`, or free space with `ramet prune`, \
                                               then rerun"
                .to_owned(),
            Self::AlreadyInitialized { .. } => "`ramet init` runs only once per project".to_owned(),
            Self::MainEnvNameRequired { name, .. } => format!(
                "the main env keeps its name whatever branch the main clone is on: confirm it \
                 with `ramet init --name {name} …`, or choose another one"
            ),
            Self::NoCommit { .. } => "a worktree only holds what is committed: commit the \
                                      project's files first (`git add -A && git commit`), \
                                      then run `ramet new` again"
                .to_owned(),
            Self::ComposeFilesNotCommitted { files, branch } => {
                uncommitted_hint(files, branch.as_deref())
            }
            Self::CurrentEnvironment { main_clone, .. } => format!(
                "move elsewhere first (for example `cd {}`), then try again",
                main_clone.display()
            ),
            Self::DeinitOutsideMainClone { main_clone, .. } => {
                format!("go to {} and try again", main_clone.display())
            }
            Self::EnvironmentsRemain { .. } => {
                "remove them first with `ramet rm <name>`, then rerun `deinit`".to_owned()
            }
            Self::ProjectDirNotRemovable { .. } => {
                "look at what it contains before insisting".to_owned()
            }
            _ => return None,
        };
        Some(hint)
    }

    /// What the user can do about an error of the data volume, if anything.
    fn volume_hint(&self) -> Option<String> {
        let hint = match self {
            Self::ForeignMount { .. } => "unmount it, or choose another mount point".to_owned(),
            Self::NotMounted { .. } => {
                "run `ramet setup`: it prepares the data volume, once per machine".to_owned()
            }
            Self::RootNotWritable { .. } => {
                "run `ramet setup`: it hands the volume over to you".to_owned()
            }
            Self::FstabLineIncomplete { .. } => {
                "add them to that line by hand: ramet never edits an existing fstab line".to_owned()
            }
            Self::FstabSourceMissing { .. } => "fix that line by hand, or remove it and run \
                                                `ramet setup` again: ramet never edits an \
                                                existing fstab line"
                .to_owned(),
            Self::RootMissing { .. } => "run `ramet doctor`".to_owned(),
            Self::VolumeNotResizable { .. } => "ramet only resizes the image `ramet setup` \
                                                creates; a btrfs of your own is yours to resize"
                .to_owned(),
            Self::VolumeTooSmall { minimum, .. } => format!(
                "the smallest size accepted is {}, 1 GiB above what it holds, for btrfs to move \
                 data around: `ramet setup --size {}`. `ramet df` shows what takes the space, \
                 `ramet prune` deletes what is orphaned.",
                crate::util::size::human_bytes(Some(*minimum)),
                crate::util::size::size_argument(minimum.div_ceil(1 << 30) << 30)
            ),
            Self::ShrinkIncomplete { .. } => {
                "`ramet doctor` shows the sizes; run the same `ramet setup --size` again".to_owned()
            }
            _ => return None,
        };
        Some(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_refusals_explain_themselves() {
        let err = Error::DeletionRefused {
            path: PathBuf::from("/tmp/x"),
            reason: DeletionRefusal::OutsideRoot {
                root: PathBuf::from("/srv/ramet"),
            },
        };
        assert_eq!(
            err.to_string(),
            "refusing to delete /tmp/x: outside of /srv/ramet"
        );
    }

    #[test]
    fn unknown_env_lists_the_known_ones() {
        let err = Error::UnknownEnvironment {
            name: "ghost".into(),
            project: "demo".into(),
            known: vec!["feat-a".into(), "main".into()],
        };
        assert_eq!(err.hint().as_deref(), Some("known envs: feat-a, main"));
    }

    #[test]
    fn insufficient_space_shows_both_sizes() {
        let err = Error::InsufficientSpace {
            needed: 100 << 30,
            available: 10 << 30,
            root: PathBuf::from("/srv/ramet"),
        };
        let message = err.to_string();
        assert!(
            message.contains("100.0 GiB") && message.contains("10.0 GiB"),
            "{message}"
        );
    }
}
