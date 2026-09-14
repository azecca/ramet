//! Test fixtures: a throwaway data root, real git repositories, and faithful
//! doubles of docker, btrfs and the host.
//!
//! Nothing here touches `/srv/ramet`: every fixture works in a temporary
//! directory and points the layout there. Git is the real one, worktrees
//! included; docker and btrfs are simulated.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use ramet::context::{Context, Services};
use ramet::env::Env;
use ramet::error::Result;
use ramet::layout::Layout;
use ramet::process::{Cmd, Output, Runner, SystemRunner};
use ramet::storage::{Btrfs, Usage};
use ramet::ui::{Streams, Style, Ui};
use serde_json::{Value, json};

/// Name of the project every fixture env belongs to.
pub(crate) const PROJECT: &str = "demo";

/// A handler answering some commands in place of the default simulation.
type Handler = Box<dyn Fn(&Cmd) -> Option<Output>>;

// ------------------------------------------------------------------ runner

/// One `docker compose` invocation, as recorded by [`FakeRunner`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeCall {
    /// Value of `-p`, when given.
    pub(crate) project: Option<String>,
    /// The compose subcommand.
    pub(crate) verb: String,
    /// Arguments after the subcommand.
    pub(crate) args: Vec<String>,
}

impl ComposeCall {
    fn parse(argv: &[String]) -> Option<Self> {
        if argv.get(..2)? != ["docker", "compose"] {
            return None;
        }
        let mut project = None;
        let mut tokens = argv[2..].iter();
        while let Some(token) = tokens.next() {
            match token.as_str() {
                "-p" | "--project-name" => project = tokens.next().cloned(),
                "--project-directory" | "-f" | "--file" | "--profile" => {
                    tokens.next();
                }
                verb => {
                    return Some(Self {
                        project,
                        verb: verb.to_owned(),
                        args: tokens.cloned().collect(),
                    });
                }
            }
        }
        None
    }
}

/// Simulates docker, delegates git to the real one, records everything.
pub(crate) struct FakeRunner {
    real: SystemRunner,
    calls: RefCell<Vec<Cmd>>,
    config: RefCell<Value>,
    failing: RefCell<BTreeSet<String>>,
    containers: RefCell<String>,
    volumes: RefCell<Vec<String>>,
    volume_bytes: Cell<u64>,
    handlers: RefCell<Vec<Handler>>,
}

impl Default for FakeRunner {
    fn default() -> Self {
        Self {
            real: SystemRunner::default(),
            calls: RefCell::default(),
            config: RefCell::new(json!({"services": {}, "volumes": {}, "networks": {}})),
            failing: RefCell::default(),
            containers: RefCell::new("[]".to_owned()),
            volumes: RefCell::default(),
            volume_bytes: Cell::new(0),
            handlers: RefCell::default(),
        }
    }
}

impl FakeRunner {
    /// What `docker compose config --format json` prints.
    pub(crate) fn set_config(&self, config: Value) {
        *self.config.borrow_mut() = config;
    }

    /// Makes a compose subcommand fail.
    pub(crate) fn fail(&self, verb: &str) {
        self.failing.borrow_mut().insert(verb.to_owned());
    }

    /// What `docker compose ps --format json` prints.
    pub(crate) fn set_containers(&self, output: &str) {
        output.clone_into(&mut self.containers.borrow_mut());
    }

    /// Docker volumes that exist.
    pub(crate) fn set_volumes(&self, names: &[&str]) {
        *self.volumes.borrow_mut() = names.iter().map(|&name| name.to_owned()).collect();
    }

    /// Size every docker volume reports.
    pub(crate) fn set_volume_bytes(&self, bytes: u64) {
        self.volume_bytes.set(bytes);
    }

    /// Answers the commands `handler` accepts, before the default simulation.
    pub(crate) fn handle(&self, handler: impl Fn(&Cmd) -> Option<Output> + 'static) {
        self.handlers.borrow_mut().push(Box::new(handler));
    }

    /// Every command run so far.
    pub(crate) fn calls(&self) -> Vec<Cmd> {
        self.calls.borrow().clone()
    }

    /// Every command run so far, as argument vectors.
    pub(crate) fn argvs(&self) -> Vec<Vec<String>> {
        self.calls().iter().map(Cmd::argv).collect()
    }

    /// Every command except git ones.
    pub(crate) fn external_argvs(&self) -> Vec<Vec<String>> {
        self.argvs()
            .into_iter()
            .filter(|argv| argv[0] != "git")
            .collect()
    }

    /// Every `docker compose` invocation.
    pub(crate) fn compose_calls(&self) -> Vec<ComposeCall> {
        self.argvs()
            .iter()
            .filter_map(|argv| ComposeCall::parse(argv))
            .collect()
    }

    /// Compose subcommands run against the stack of `env`.
    pub(crate) fn verbs_for(&self, env: &str) -> Vec<String> {
        let project = format!("{PROJECT}-{env}");
        self.compose_calls()
            .into_iter()
            .filter(|call| call.project.as_deref() == Some(project.as_str()))
            .map(|call| call.verb)
            .collect()
    }

    /// Invocations of `docker compose ... config`.
    pub(crate) fn config_calls(&self) -> Vec<Cmd> {
        self.calls()
            .into_iter()
            .filter(|cmd| ComposeCall::parse(&cmd.argv()).is_some_and(|call| call.verb == "config"))
            .collect()
    }

    fn docker(&self, argv: &[String]) -> Output {
        let arg = |index: usize| argv.get(index).map_or("", String::as_str);
        match arg(1) {
            "compose" => self.compose(argv),
            "volume" if arg(2) == "inspect" => {
                if self.volumes.borrow().iter().any(|name| name == arg(3)) {
                    Output::success_with("[]")
                } else {
                    Output::failure(1, "Error: no such volume")
                }
            }
            "volume" if arg(2) == "ls" => Output::success_with(self.volumes.borrow().join("\n")),
            "run" if argv.iter().any(|a| a == "du") => {
                Output::success_with(format!("{}\t/v\n", self.volume_bytes.get() / 1024))
            }
            _ => Output::success_with(""),
        }
    }

    fn compose(&self, argv: &[String]) -> Output {
        let Some(call) = ComposeCall::parse(argv) else {
            return Output::success_with("");
        };
        if self.failing.borrow().contains(&call.verb) {
            return Output::failure(1, format!("simulated failure of compose {}", call.verb));
        }
        match call.verb.as_str() {
            "config" => Output::success_with(self.config.borrow().to_string()),
            "ps" => Output::success_with(self.containers.borrow().clone()),
            _ => Output::success_with(""),
        }
    }
}

impl Runner for FakeRunner {
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        self.calls.borrow_mut().push(cmd.clone());
        for handler in self.handlers.borrow().iter() {
            if let Some(output) = handler(cmd) {
                return Ok(output);
            }
        }
        let argv = cmd.argv();
        match argv[0].as_str() {
            "git" => self.real.run(cmd),
            "docker" => Ok(self.docker(&argv)),
            _ => Ok(Output::failure(1, "not simulated")),
        }
    }
}

// ------------------------------------------------------------------- btrfs

/// What the fake btrfs was asked to do.
#[derive(Debug, Default)]
pub(crate) struct BtrfsLog {
    /// Snapshots, as (source name, destination name, read-only).
    pub(crate) snapshots: Vec<(String, String, bool)>,
    /// Names of deleted subvolumes, in order.
    pub(crate) deleted: Vec<String>,
    /// Usage reported for every subvolume.
    pub(crate) usage: Usage,
    /// Usage reported for the directories of these names, instead.
    pub(crate) usage_of: BTreeMap<String, Usage>,
    /// Ids of the subvolumes of these names; the others have none.
    pub(crate) ids: BTreeMap<String, u64>,
    /// Makes every snapshot fail.
    pub(crate) fail_snapshots: bool,
    /// Makes every deletion fail, as an interruption would leave it.
    pub(crate) fail_deletes: bool,
}

/// A faithful btrfs double: a snapshot really copies, a deletion really
/// deletes, and every directory counts as a subvolume. Without that, a stale
/// env.json could survive a restore without any test noticing.
#[derive(Clone, Default)]
pub(crate) struct FakeBtrfs(pub Rc<RefCell<BtrfsLog>>);

fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Btrfs for FakeBtrfs {
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).map_err(|source| ramet::error::Error::io(path, source))
    }

    fn snapshot(&self, source: &Path, destination: &Path, read_only: bool) -> Result<()> {
        if self.0.borrow().fail_snapshots {
            return Err(ramet::error::Error::CommandFailed {
                command: "btrfs subvolume snapshot".to_owned(),
                detail: "simulated failure".to_owned(),
            });
        }
        self.0
            .borrow_mut()
            .snapshots
            .push((name_of(source), name_of(destination), read_only));
        copy_tree(source, destination).map_err(|err| ramet::error::Error::io(destination, err))
    }

    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        if self.0.borrow().fail_deletes {
            return Err(ramet::error::Error::CommandFailed {
                command: "btrfs subvolume delete".to_owned(),
                detail: "simulated failure".to_owned(),
            });
        }
        self.0.borrow_mut().deleted.push(name_of(path));
        let _ = fs::remove_dir_all(path);
        Ok(())
    }

    fn is_subvolume(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn usage(&self, path: &Path) -> Usage {
        let log = self.0.borrow();
        log.usage_of
            .get(&name_of(path))
            .copied()
            .unwrap_or(log.usage)
    }

    fn subvolume_id(&self, path: &Path) -> Option<u64> {
        self.0.borrow().ids.get(&name_of(path)).copied()
    }
}

// -------------------------------------------------------------------- host

/// Host facts under the test's control.
pub(crate) struct HostState {
    /// Size of the filesystem holding any path.
    pub(crate) fs_size: Cell<u64>,
    /// Free bytes reported for any path.
    pub(crate) free_bytes: Cell<u64>,
    /// Environment variables.
    pub(crate) vars: RefCell<HashMap<String, String>>,
    /// Effective user id.
    pub(crate) euid: Cell<u32>,
    /// Programs that are not installed; every other one is, in `/usr/bin`.
    pub(crate) missing_programs: RefCell<BTreeSet<String>>,
    /// Where sysfs is.
    pub(crate) sysfs: RefCell<PathBuf>,
}

impl Default for HostState {
    fn default() -> Self {
        Self {
            fs_size: Cell::new(1 << 40),
            free_bytes: Cell::new(500 << 30),
            vars: RefCell::default(),
            euid: Cell::new(1000),
            missing_programs: RefCell::default(),
            sysfs: RefCell::new(PathBuf::from("/nonexistent/sys")),
        }
    }
}

/// A host whose answers come from a [`HostState`].
#[derive(Clone, Default)]
pub(crate) struct FakeHost(pub Rc<HostState>);

impl ramet::host::Host for FakeHost {
    fn find_program(&self, program: &str) -> Option<PathBuf> {
        (!self.0.missing_programs.borrow().contains(program))
            .then(|| Path::new("/usr/bin").join(program))
    }

    fn space(&self, _path: &Path) -> io::Result<ramet::host::Space> {
        let free = self.0.free_bytes.get();
        Ok(ramet::host::Space {
            size: self.0.fs_size.get(),
            free,
            available: free,
        })
    }

    fn port_is_free(&self, _port: u16) -> bool {
        true
    }

    fn var(&self, name: &str) -> Option<String> {
        self.0.vars.borrow().get(name).cloned()
    }

    fn user_name(&self) -> String {
        "tester".to_owned()
    }

    fn effective_uid(&self) -> u32 {
        self.0.euid.get()
    }

    fn sysfs(&self) -> PathBuf {
        self.0.sysfs.borrow().clone()
    }
}

// ----------------------------------------------------------------- machine

/// The system side of the data volume, as `setup` and `doctor` see it.
#[derive(Debug, Default)]
pub(crate) struct Machine {
    /// Source and options of the `/etc/fstab` line for the data root.
    pub(crate) fstab: Option<(String, String)>,
    /// Everything appended to `/etc/fstab` as root.
    pub(crate) appended: String,
    /// Whether btrfs is mounted on the data root.
    pub(crate) mounted: bool,
    /// Makes sudo fail, as after three wrong passwords.
    pub(crate) sudo_fails: bool,
}

impl Fixture {
    /// Simulates the machine around the data volume: `findmnt`, `mount`,
    /// `mkfs.btrfs`, and what root does, directly or through sudo.
    pub(crate) fn machine(&self) -> Rc<RefCell<Machine>> {
        let machine = Rc::new(RefCell::new(Machine::default()));
        let state = Rc::clone(&machine);
        let root = self.root.clone();
        self.runner
            .handle(move |cmd| simulate_machine(&state, &root, cmd));
        machine
    }

    /// Source and options of a complete fstab line mounting the fixture's image.
    pub(crate) fn image_fstab_line(&self) -> (String, String) {
        (
            self.layout().data_image().display().to_string(),
            ramet::layout::FSTAB_OPTIONS.to_owned(),
        )
    }

    /// Commands run as root, through sudo or directly, as argument vectors.
    pub(crate) fn root_commands(&self) -> Vec<Vec<String>> {
        self.runner
            .argvs()
            .into_iter()
            .filter(|argv| ["sudo", "mkdir", "tee", "chown"].contains(&argv[0].as_str()))
            .collect()
    }
}

/// Where the quota groups of the test volume's btrfs are, under sysfs.
const QGROUPS: &str = "fs/btrfs/0000-test/qgroups";

/// ramet's image and the layers built on it, as the resize double keeps them.
#[derive(Debug, Default)]
pub(crate) struct ImageState {
    /// Size of the loop device.
    pub(crate) device: u64,
    /// Makes `btrfs filesystem resize` fail, as when data cannot move.
    pub(crate) resize_fails: bool,
    /// Makes `btrfs filesystem resize` report success without resizing.
    pub(crate) resize_ignored: bool,
}

impl Fixture {
    /// A data volume mounted from ramet's image, sparse, of `file` bytes, on
    /// a loop device of `device` bytes, holding a btrfs of `filesystem`
    /// bytes with `used` in use. `findmnt`, `losetup`, `lsblk` and the resize
    /// commands root runs are simulated; the image file is real.
    pub(crate) fn image_volume(
        &self,
        file: u64,
        device: u64,
        filesystem: u64,
        used: u64,
    ) -> Rc<RefCell<ImageState>> {
        let image = self.layout().data_image().to_owned();
        fs::File::create(&image)
            .and_then(|handle| handle.set_len(file))
            .expect("image");
        let state = Rc::new(RefCell::new(ImageState {
            device,
            ..ImageState::default()
        }));
        let host = Rc::clone(&self.host.0);
        host.fs_size.set(filesystem);
        host.free_bytes.set(filesystem - used);
        let shared = Rc::clone(&state);
        // Registered before the machine's handler, which it answers ahead of.
        self.runner.handle(move |cmd| {
            let argv = cmd.argv();
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let argv = argv.strip_prefix(&["sudo"]).unwrap_or(&argv);
            let size = |text: &str| ramet::util::size::parse_size(text).expect("a size");
            let mut state = shared.borrow_mut();
            match argv {
                ["losetup", "-n", "-O", "BACK-FILE", _] => {
                    Some(Output::success_with(format!("{}\n", image.display())))
                }
                ["lsblk", .., _device] => Some(Output::success_with(format!("{}\n", state.device))),
                ["btrfs", "filesystem", "resize", new, _root] => {
                    if state.resize_fails {
                        return Some(Output::failure(
                            1,
                            "ERROR: unable to resize '/srv/ramet': No space left on device",
                        ));
                    }
                    if !state.resize_ignored {
                        host.fs_size.set(size(new));
                        host.free_bytes.set(size(new) - used);
                    }
                    Some(Output::success_with(""))
                }
                ["truncate", "-s", new, path] => {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .and_then(|handle| handle.set_len(size(new)))
                        .expect("truncate");
                    Some(Output::success_with(""))
                }
                ["losetup", "-c", _device] => {
                    state.device = fs::metadata(&image).expect("image").len();
                    Some(Output::success_with(""))
                }
                ["btrfs", "quota", verb, .., _root] => {
                    let qgroups = host.sysfs.borrow().join(QGROUPS);
                    fs::create_dir_all(&qgroups).expect("qgroups");
                    let flag = if *verb == "enable" { "1\n" } else { "0\n" };
                    fs::write(qgroups.join("inconsistent"), flag).expect("inconsistent");
                    Some(Output::success_with(""))
                }
                _ => None,
            }
        });
        let machine = self.machine();
        machine.borrow_mut().fstab = Some(self.image_fstab_line());
        machine.borrow_mut().mounted = true;
        state
    }

    /// A sysfs where the volume's btrfs, on `/dev/loop0` as the machine
    /// double mounts it, has quotas off.
    pub(crate) fn quotas_off(&self) -> PathBuf {
        let sysfs = self.base.join("sys");
        fs::create_dir_all(sysfs.join(QGROUPS).with_file_name("devices/loop0")).expect("sysfs");
        self.host.0.sysfs.borrow_mut().clone_from(&sysfs);
        sysfs
    }

    /// A sysfs where the volume's btrfs, on `/dev/loop0` as the machine
    /// double mounts it, has quotas on, counting `(subvolume, referenced,
    /// exclusive)` for each of `groups`. Returns the `qgroups` directory.
    pub(crate) fn quotas(&self, groups: &[(&str, u64, u64)]) -> PathBuf {
        let qgroups = self.quotas_off().join(QGROUPS);
        fs::create_dir_all(&qgroups).expect("qgroups");
        fs::write(qgroups.join("inconsistent"), "0\n").expect("inconsistent");
        for (id, (name, referenced, exclusive)) in (256..).zip(groups) {
            let group = qgroups.join(format!("0_{id}"));
            fs::create_dir_all(&group).expect("qgroup");
            fs::write(group.join("referenced"), format!("{referenced}\n")).expect("referenced");
            fs::write(group.join("exclusive"), format!("{exclusive}\n")).expect("exclusive");
            self.btrfs.0.borrow_mut().ids.insert((*name).to_owned(), id);
        }
        qgroups
    }

    /// Size of the image file.
    pub(crate) fn image_size(&self) -> u64 {
        fs::metadata(self.layout().data_image())
            .expect("image")
            .len()
    }
}

fn simulate_machine(machine: &RefCell<Machine>, root: &Path, cmd: &Cmd) -> Option<Output> {
    let argv = cmd.argv();
    let has = |flag: &str| argv.iter().any(|arg| arg == flag);
    let program = Path::new(&argv[0]).file_name()?.to_str()?.to_owned();
    match program.as_str() {
        "findmnt" => Some(findmnt(&machine.borrow(), &has)),
        "mount" => {
            let mut state = machine.borrow_mut();
            Some(if state.fstab.is_some() && root.is_dir() {
                state.mounted = true;
                Output::success_with("")
            } else {
                Output::failure(32, "mount: /srv/ramet: can't find in /etc/fstab.")
            })
        }
        "mkfs.btrfs" => Some(Output::success_with("")),
        "sudo" if machine.borrow().sudo_fails => {
            Some(Output::failure(1, "sudo: 3 incorrect password attempts"))
        }
        "sudo" => Some(as_root(machine, root, &argv[1..], cmd.input_text())),
        "mkdir" | "tee" | "chown" => Some(as_root(machine, root, &argv, cmd.input_text())),
        _ => None,
    }
}

fn findmnt(machine: &Machine, has: &dyn Fn(&str) -> bool) -> Output {
    let found = |text: &str| Output::success_with(format!("{text}\n"));
    let none = || Output::failure(1, "");
    if has("--fstab") {
        return match &machine.fstab {
            Some((source, _)) if has("SOURCE") => found(source),
            Some((_, options)) if has("OPTIONS") => found(options),
            _ => none(),
        };
    }
    match (machine.mounted, has("FSTYPE"), has("--target")) {
        (true, true, _) => found("btrfs"),
        (false, true, true) => found("ext4"),
        (true, false, _) if has("OPTIONS") => found(ramet::layout::FSTAB_OPTIONS),
        (true, false, _) => found("/dev/loop0"),
        _ => none(),
    }
}

fn as_root(
    machine: &RefCell<Machine>,
    root: &Path,
    argv: &[String],
    input: Option<&str>,
) -> Output {
    match argv[0].as_str() {
        "mkdir" => fs::create_dir_all(argv.last().expect("a directory")).expect("mkdir"),
        "tee" => {
            let text = input.unwrap_or_default();
            let mut state = machine.borrow_mut();
            state.appended.push_str(text);
            let line = text
                .lines()
                .rfind(|line| !line.trim().is_empty() && !line.starts_with('#'))
                .expect("an fstab line");
            let fields: Vec<&str> = line.split_whitespace().collect();
            state.fstab = Some((fields[0].to_owned(), fields[3].to_owned()));
        }
        "chown" => make_writable(root),
        other => panic!("unexpected root command {other}"),
    }
    Output::success_with("")
}

/// Takes write permission away from `path`. Returns false when that changes
/// nothing, as for root, who ignores permissions: the test cannot run then.
pub(crate) fn make_read_only(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555)).expect("chmod");
    let effective = !ramet::util::fs::is_writable(path);
    if !effective {
        make_writable(path);
    }
    effective
}

/// Gives write permission on `path` back.
pub(crate) fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

// ---------------------------------------------------------------- terminal

/// An in-memory output stream shared between the fixture and its contexts.
#[derive(Clone, Default)]
pub(crate) struct SharedBuffer(Rc<RefCell<Vec<u8>>>);

impl SharedBuffer {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ----------------------------------------------------------------- fixture

/// A temporary world: data root, main clone, fakes and captured streams.
pub(crate) struct Fixture {
    _tmp: tempfile::TempDir,
    /// Canonical temporary directory everything lives in.
    pub(crate) base: PathBuf,
    /// The data root.
    pub(crate) root: PathBuf,
    /// The main clone, a real git repository.
    pub(crate) clone: PathBuf,
    /// Docker and git.
    pub(crate) runner: Rc<FakeRunner>,
    /// btrfs.
    pub(crate) btrfs: FakeBtrfs,
    /// The host.
    pub(crate) host: FakeHost,
    stdout: SharedBuffer,
    stderr: SharedBuffer,
    input: RefCell<String>,
    stdin_is_terminal: Cell<bool>,
    stderr_is_terminal: Cell<bool>,
}

impl Fixture {
    /// A data root and a main clone holding an empty `compose.yml`.
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("temporary directory");
        let base = fs::canonicalize(tmp.path()).expect("canonical temporary directory");
        let root = base.join("srv");
        fs::create_dir(&root).expect("data root");
        let clone = git_repo(&base.join("app"), &[("compose.yml", "services: {}\n")]);
        Self {
            _tmp: tmp,
            base,
            root,
            clone,
            runner: Rc::default(),
            btrfs: FakeBtrfs::default(),
            host: FakeHost::default(),
            stdout: SharedBuffer::default(),
            stderr: SharedBuffer::default(),
            input: RefCell::default(),
            stdin_is_terminal: Cell::new(false),
            stderr_is_terminal: Cell::new(false),
        }
    }

    /// Like [`Fixture::new`], with the env `main` of the main clone recorded.
    pub(crate) fn with_main_env() -> Self {
        let fixture = Self::new();
        let clone = fixture.clone.clone();
        fixture.save_env(fixture.env("main", |env| env.worktree = clone));
        fixture
    }

    /// The layout of the fixture.
    pub(crate) fn layout(&self) -> Layout {
        Layout::new(&self.root, self.base.join("data.img"))
    }

    /// A context whose working directory is the main clone.
    pub(crate) fn ctx(&self) -> Context {
        self.ctx_at(&self.clone)
    }

    /// A context whose working directory is `cwd`.
    pub(crate) fn ctx_at(&self, cwd: &Path) -> Context {
        let input = self.input.borrow().clone();
        let ui = Ui::new(
            Style::plain(),
            Streams {
                stdout: Box::new(self.stdout.clone()),
                stderr: Box::new(self.stderr.clone()),
                stdin: Box::new(Cursor::new(input)),
                stdin_is_terminal: self.stdin_is_terminal.get(),
                stderr_is_terminal: self.stderr_is_terminal.get(),
            },
        );
        let runner: Rc<dyn Runner> = self.runner.clone();
        Context::new(
            self.layout(),
            cwd.to_owned(),
            ui,
            Services {
                runner,
                btrfs: Box::new(self.btrfs.clone()),
                host: Box::new(self.host.clone()),
            },
        )
    }

    /// Simulates a terminal whose user types `input`.
    pub(crate) fn answer(&self, input: &str) {
        input.clone_into(&mut self.input.borrow_mut());
        self.stdin_is_terminal.set(true);
    }

    /// Declares standard error a terminal.
    pub(crate) fn stderr_is_terminal(&self) {
        self.stderr_is_terminal.set(true);
    }

    /// Everything written to standard output so far.
    pub(crate) fn stdout(&self) -> String {
        self.stdout.contents()
    }

    /// Everything written to standard error so far.
    pub(crate) fn stderr(&self) -> String {
        self.stderr.contents()
    }

    /// An env of [`PROJECT`] with sensible defaults, adjusted by `customize`.
    pub(crate) fn env(&self, name: &str, customize: impl FnOnce(&mut Env)) -> Env {
        let mut env: Env = serde_json::from_value(json!({
            "name": name,
            "project": PROJECT,
            "parent": null,
            "worktree": self.base.join("wt"),
            "branch_at_creation": "main",
            "created_at": "2026-01-01T00:00:00+00:00",
            "ports": {"range": null, "map": {}},
            "checkpoints": {},
        }))
        .expect("valid env");
        customize(&mut env);
        env
    }

    /// Creates the env's directory under the data root and writes its env.json.
    pub(crate) fn save_env(&self, env: Env) -> Env {
        let layout = self.layout();
        fs::create_dir_all(env.dir(&layout)).expect("env directory");
        env.save(&layout).expect("env.json");
        env
    }

    /// Reads back the env.json of `name`.
    pub(crate) fn load(&self, name: &str) -> Env {
        ramet::env::store::load_envs(&self.layout(), PROJECT)
            .remove(name)
            .unwrap_or_else(|| panic!("env {name} not found"))
    }

    /// Names of the envs on disk.
    pub(crate) fn env_names(&self) -> Vec<String> {
        ramet::env::store::load_envs(&self.layout(), PROJECT)
            .into_keys()
            .collect()
    }

    /// A checkpoint directory holding a frozen copy of its env's env.json,
    /// as a real snapshot would.
    pub(crate) fn checkpoint_dir(&self, env: &str, label: &str) -> PathBuf {
        let layout = self.layout();
        let path = layout.checkpoint_dir(PROJECT, env, label);
        fs::create_dir_all(&path).expect("checkpoint directory");
        let frozen = self.env(env, |_| {});
        ramet::util::fs::write_json(&path.join("env.json"), &frozen).expect("frozen env.json");
        path
    }

    /// Adds a worktree `app.wt/<name>` on a new branch of the same name.
    pub(crate) fn add_worktree(&self, name: &str) -> PathBuf {
        let path = self.base.join("app.wt").join(name);
        git(
            &self.clone,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                path.to_str().expect("utf-8"),
            ],
        );
        path
    }

    /// Records `name` as a secondary env living in a real worktree.
    pub(crate) fn secondary_env(&self, name: &str) -> Env {
        let worktree = self.add_worktree(name);
        self.save_env(self.env(name, |env| {
            env.parent = Some("main".to_owned());
            env.worktree = worktree;
        }))
    }
}

// --------------------------------------------------------------------- git

/// Runs the real git in `dir`, panicking on failure.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=ramet",
            "-c",
            "user.email=ramet@test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A standalone repository with one commit holding `files`.
pub(crate) fn git_repo(path: &Path, files: &[(&str, &str)]) -> PathBuf {
    for (relative, content) in files {
        let target = path.join(relative);
        fs::create_dir_all(target.parent().expect("parent")).expect("directory");
        fs::write(target, content).expect("file");
    }
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "initial"]);
    path.to_owned()
}

/// Writes `settings` as the `.ramet.json` of `worktree`, untracked.
pub(crate) fn write_settings(worktree: &Path, settings: &Value) {
    fs::write(
        worktree.join(ramet::settings::FILE_NAME),
        settings.to_string(),
    )
    .expect(".ramet.json");
}

/// Creates `relative` under `dir` with `content`, and its directories.
pub(crate) fn write_file(dir: &Path, relative: &str, content: &str) {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    fs::write(path, content).expect("file");
}

/// The full text of an error: message and hint.
pub(crate) fn full_message(err: &ramet::error::Error) -> String {
    match err.hint() {
        Some(hint) => format!("{err}\n{hint}"),
        None => err.to_string(),
    }
}

/// A published port specification as `docker compose config` prints it.
pub(crate) fn port(target: u16, published: u16) -> Value {
    json!({"mode": "ingress", "target": target, "published": published.to_string(), "protocol": "tcp"})
}
