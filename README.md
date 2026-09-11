# ramet

One git branch, one docker compose stack, data of its own.

In botany, a ramet is an individual born from a clone: an offshoot that has
taken root and lives its own life, apart from the mother plant. ramet does the
same with a docker compose project. `ramet new feat-x` creates a git worktree,
a btrfs snapshot of the current environment's volumes, and an isolated compose
stack. A developer or an AI agent works there without touching anything else;
`ramet checkpoint` sets a return point, `ramet restore` goes back to it, and
`ramet rm` throws everything away.

The project itself is never modified: no file is added to the repository, the
`compose.yml` stays as it is, and a plain `docker compose up` keeps working.
The only file ramet reads from the project is an optional `.ramet.json`,
written by you, for projects that need it.

## Requirements

- Linux, Docker Engine with Compose v2 (`docker compose`), and a user in the
  `docker` group
- git 2.5 or later (worktrees)
- `btrfs-progs`, and util-linux (`findmnt`, `mount`)

## Installation

From a clone of the repository, with Rust installed:

```sh
./install.sh
```

The script builds ramet, puts it in `~/.local/bin`, then runs `ramet setup`,
which prepares the data volume:

1. a sparse 10 GiB btrfs image in `~/.local/share/ramet/data.img`, which only
   occupies what it contains (`ramet setup --size 30G` for another size);
2. the mount point `/srv/ramet` and a `noauto,user` line in `/etc/fstab`, so
   that ramet mounts the volume by itself, without privilege, and nothing is
   mounted at boot;
3. btrfs quotas on the image, which let `ramet df` tell what each env takes.

The last two steps need root: `ramet setup` shows the commands, then runs them
through `sudo`, which asks for your password once. It never edits an existing
fstab line. `ramet setup` is the only command that asks for privilege, here
and when you resize the volume; every other command refuses to run under
`sudo`.

Running `./install.sh` again updates ramet; `ramet setup` skips whatever is
already in place. `ramet doctor` checks the whole installation at any time.

## Commands

```
ramet setup [--size <size>]  once per machine: prepare the data volume; resize it
ramet init                   once per project: migrate the existing docker volumes
ramet deinit                 undo init: hand the project back, repository untouched
ramet new <name>             clone the current env (worktree + snapshot + stack)
ramet ls [--json]            envs of the project: parent, branch, state, ports
ramet checkpoint <label>     set a return point on the env's data
ramet log [--json]           checkpoints of the current env
ramet restore <label>        rewind the env's data to a checkpoint
ramet rm <name>              remove an env, its worktree and its checkpoints
ramet sync [--from <env>]    copy the local files (.env…) of another env again
ramet df [--json]            how full the data volume is, and what fills it
ramet prune                  delete the envs and checkpoints whose worktree is gone
ramet path [<name>]          worktree of an env, for `cd`
ramet prompt [--short]       `project:env` marker for the shell prompt
ramet doctor                 prerequisites and inconsistencies
ramet compose <command> …    docker compose, on the current env's stack
```

`ramet <command> --help` describes every option.

Docker compose commands go through `ramet compose`, their arguments untouched:
`ramet compose up -d`, `ramet compose logs -f web`, `ramet compose exec db
psql`. ramet adds the env's project name and generated configuration, so each
command reaches the env's own stack. Keeping them apart means no ramet command
hides a compose one: `ramet rm` removes an env, `ramet compose rm` removes
stopped containers. For a shorter form, `alias rc='ramet compose'`.

The env of the main clone is named after the repository's default branch
(`main`, `master`…), whatever branch is checked out when `ramet init` runs: if
it is another one, `init` asks first. `ramet init --name` chooses another name.

## Local files

A `.env` is ignored by git, so `git worktree add` leaves it behind. `ramet new`
copies it into the new env, with every `localhost:<port>` pointing at the
source env's ports rewritten to the new env's: otherwise the application of
the new env would silently write into its neighbour's database. By default,
every `.env` and `.env.*` git does not track is copied, wherever it is in the
repository (`.env.local`, `apps/api/.env`…).

Files drift apart afterwards: a variable added to the main env's `.env` never
reaches the envs cloned before. `ramet sync`, run in an env, copies the files
again from the env it was cloned from (or from any env with `--from`), ports
rewritten. A missing file is copied; a file that differs is listed and replaced
only once you confirm (`--yes` in a script). Nothing is ever deleted, and no
file git tracks is ever written. Containers read these files when they are
created: `ramet compose up -d` applies the changes.

## `.ramet.json`

Most projects need no configuration. For the others, a `.ramet.json` at the
root of the repository says how compose runs the project and which local files
to sync. Every key is optional:

```json
{
  "compose": {
    "files": ["docker/compose/base.yml", "docker/compose/dev.yml"],
    "profiles": ["dev"]
  },
  "sync": ["**/.env", "config/local.toml", "certs"]
}
```

- `compose.files`: the compose files, in the order of `-f` options, for a
  project that runs `docker compose -f docker/compose/base.yml -f
  docker/compose/dev.yml up` rather than a plain `docker compose up`. As with
  `-f`, relative paths in the files and the `.env` compose reads are those of
  the first file's directory.
- `compose.profiles`: the profiles every command enables.
  `ramet compose --profile tools run migrate` adds one for a single command.
- `sync`: the local files to sync, as paths or glob patterns (`*` within a
  directory, `**` across directories); a directory stands for everything in
  it. It replaces the default (`["**/.env", "**/.env.*"]`): list the `.env`
  files too if you want to keep them, or write `[]` to sync nothing. Only files
  git does not track are synced, and directories git ignores as a whole, such
  as `node_modules/`, are searched only by a pattern that names them.

Commit the file with the project, or keep it to yourself: when git does not
track it, `ramet new` copies it into every new env like the other local files.
ramet reads the `.ramet.json` of each env's own worktree, so a branch that
reorganizes its compose files brings its own. An unknown key is an error, not
a silently ignored setting.

## Disk space

`ramet df` shows how full the data volume is and what fills it: for each env
and checkpoint, what it holds and what it holds alone. A clone shares every
file it has not changed with the env it came from, so a fresh `ramet new`
costs almost nothing; what an env holds alone is what deleting it frees. The
figures come from btrfs quotas, which `ramet setup` turns on for ramet's
image. Without them, directories unreadable without root (a database's) are
left out, and the figures are lower bounds, marked `≥`.

The image is sparse: it only takes from the host disk what it holds, and the
space freed inside returns to the disk, in large enough pieces. Blocks freed
one by one stay in the image for btrfs to reuse; when they add up, `ramet df`
suggests `sudo fstrim /srv/ramet`, which hands them back. The image's size is
a ceiling, which `ramet setup --size` moves, online:

```sh
ramet setup --size 20G   # grow the volume
ramet setup --size 8G    # shrink it, as long as what it holds fits
```

Resizing needs root: setup shows the commands, then runs them through sudo.
Shrinking has btrfs move its data out of the end of the volume before the
image is cut, and is refused below what the volume holds plus 1 GiB. An
interrupted resize is completed by running the same command again.

`ramet prune` deletes what no longer belongs to anything, across every
project: the envs and checkpoints whose worktree is gone, deleted by hand or
with the whole repository, and the remains of an interrupted `ramet init`. It
lists them with the space they take, stops their stacks, and deletes them only
once you confirm (`--yes` in a script). A repository you moved looks orphaned
too, since its envs recorded the old path: the listing shows that path, answer
no. What might still be wanted is left alone and reported by `ramet doctor`:
an unreadable `env.json`, or a volume directory the compose file no longer
declares, which another branch of the same worktree may still use.

## Knowing where you are

Like git with branches, ramet deduces the env from the working directory. That
is convenient, and it is the main trap: a command run from the wrong worktree
acts on the wrong environment. `ramet compose` names its target on standard
error (`· shop/feat-price (feat-price)`), `ramet ls` says which env its star
designates, and the env can go into the shell prompt.

For bash or zsh, in `~/.bashrc` or `~/.zshrc`:

```sh
rcd()  { cd "$(ramet path "$1")" || return; }   # rcd feat-price
rprompt() { local e; e=$(ramet prompt) && [ -n "$e" ] && echo "($e) "; }
PS1='$(rprompt)'$PS1
```

`ramet prompt` prints nothing and exits with 0 outside an env, and never mounts
the data volume: opening a shell has no side effect.

## AI agents

An agent uses ramet like a developer does, without a terminal: `ls`, `log` and
`df` take `--json`, and a command that needs a confirmation refuses rather than
guess, until `--yes` says so. `skills/ramet/SKILL.md` tells an agent the rest:
where it stands, `ramet compose` rather than `docker compose`, an env per task,
a checkpoint before a migration, and which commands destroy what, so that it
asks before passing `--yes`.

The file follows the Agent Skills format. For Claude Code, link it among your
personal skills, from the clone of the repository:

```sh
mkdir -p ~/.claude/skills && ln -s "$PWD/skills/ramet" ~/.claude/skills/ramet
```

Other agents that read skills take the same directory in theirs. The skill
then applies in every project, without adding a file to any of them.

## Development

Rust 1.98 or later is required (`rust-version` in `Cargo.toml`). `AGENTS.md`
sums up the rules below for AI coding agents.

```sh
cargo test                                             # unit and integration tests
CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets   # pedantic lints: zero warnings
cargo fmt --check
```

`CARGO_BUILD_WARNINGS=deny` (Cargo's `build.warnings`) turns every warning of
this crate into an error without touching dependencies; it is the check to run
in CI.

### Layout

| module | role |
|---|---|
| `cli`, `app` | command line parsing; common checks (sudo guard, mount) and dispatch |
| `commands` | one module per command, each with its arguments and a `run` function |
| `env` | the domain: `env.json`, discovery of the current env, inventory of the data root, synced files |
| `settings` | the project's optional `.ramet.json` |
| `compose` | resolution of the compose configuration and its rewriting for an env |
| `ports` | stable port allocation and rewriting of ports in synced files |
| `storage` | btrfs subvolumes (deletion guarded), their sizes, and the data volume |
| `git`, `docker` | the external tools ramet drives |
| `process` | the single gateway every external command goes through |
| `context` | what a command needs from the outside world, injected |
| `error` | every anticipated failure, with its message and hint |

Commands never reach for the process environment directly: the working
directory, the terminal, external programs, btrfs and host queries all come
from a `Context`. Tests build one with faithful doubles, so the whole command
logic runs without btrfs, docker or root, against real git repositories.

### Tests

- **Unit tests** live next to the code they cover (`#[cfg(test)]` modules).
- **Integration tests** (`tests/integration/`) run each command against a
  temporary data root, real git worktrees, and simulated docker and btrfs.
  `tests/interrupt.rs` sends a real Ctrl-C, in a test binary of its own.
- **The reference scenario** (`tests/scenario.sh`) exercises
  real btrfs, real docker and real data. Without touching the host, it runs in
  a disposable docker-in-docker bench, which only needs docker and the musl
  target (the bench runs Alpine, hence a static binary):

  ```sh
  rustup target add x86_64-unknown-linux-musl
  tests/lab.sh up     # the bench: nested docker, btrfs-progs, a user without sudo
  tests/lab.sh run    # builds ramet, installs it with install.sh, then doctor and the scenario
  tests/lab.sh down
  ```

  `tests/lab.sh run --upto 3` stops after step 3; `tests/README.md` has the
  details.
