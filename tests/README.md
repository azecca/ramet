# Tests

Two complementary levels.

- **`cargo test`**: unit and integration tests. No btrfs, no docker, no root:
  they run anywhere in a few seconds, and cover most of ramet's decisions.
- **`tests/scenario.sh`**: the reference scenario. It exercises
  real btrfs, real docker and real data: only it proves that isolation works.

No test ever touches `/srv/ramet`: every fixture works in a throwaway
directory and points the data root there.

## Unit and integration tests

```sh
cargo test                    # everything
cargo test --test integration # the integration tests only
cargo test ports::            # the tests of one module
```

| where | covers |
|---|---|
| `#[cfg(test)]` modules in `src/` | each module on its own: configuration rewriting, port allocation, deletion guard, parsing of external tool output |
| `integration/` | each command end to end, against a temporary data root, **real** git repositories (worktrees included), and faithful doubles of docker, btrfs and the host |
| `interrupt.rs` | Ctrl-C while a stack is frozen, with a real signal: a test binary of its own, since signal handling is process-wide |

### What they do not cover

Everything that needs a real subvolume or a real container: subvolume
detection on a positive case, the migration of docker volumes by `ramet init`,
and the actual isolation of data between envs. That is the scenario's job.

## Reference scenario

The scenario needs a real btrfs, hence a mount, hence root. Rather than asking
for the user's password and mounting anything on the machine, `lab.sh` runs the
whole thing in a privileged docker-in-docker container: real btrfs, nested
docker daemon, real postgres and nginx containers. The host only sees a
container.

```sh
rustup target add x86_64-unknown-linux-musl   # once: the bench runs Alpine
tests/lab.sh up             # creates the bench: nested docker, btrfs-progs, a user `dev`
tests/lab.sh run            # builds ramet, installs it with install.sh, then resize, doctor and the 13 steps
tests/lab.sh run --upto 3   # stops the scenario after step 3
tests/lab.sh shell          # a shell in the bench, to dig around
tests/lab.sh down           # removes the bench and everything in it
```

Host prerequisites: docker usable without sudo, and the Rust musl target.

**The bench runs as an unprivileged user**, and `sudo` is not even installed
in it. That is what gives the result its value: if the scenario passes, ramet
needs no privilege. `install.sh` runs as that user, as it would on a real
machine; without sudo, `ramet setup` prints the root steps instead of running
them. `lab.sh` then plays the administrator (creates `/srv/ramet`, adds the
fstab line), and a second `ramet setup` mounts the volume, as the user.

Two artifices remain, specific to containers: the `/dev/loopN` node, which a
container's `/dev` lacks and which has to be created by hand, and the setuid
bit of `mount`, which lets a user mount a `user` fstab line and which the bench
sets by hand, as desktop distributions do.
The data image lives in `/var/lib/ramet-lab/` (`XDG_DATA_HOME`) rather than in
the user's home, a path no real machine uses.

### What the scenario does

It cleans up the previous state on every start, then builds a standalone git
repository in `tests/.tmp/example/` (a copy of `example/`): the main clone is
the `main` env, worktrees go to `tests/.tmp/example.wt/`, data to
`/srv/ramet/example/`. `example/` itself is never a worktree: it is a
subdirectory of the ramet repository, and `git worktree add` would create a
worktree of ramet there.

It fails loudly at the first step that does not pass, and refuses to run unless
`/srv/ramet` is backed by the bench's image `/var/lib/ramet-lab/ramet/data.img`: it destroys
everything under `/srv/ramet/example/`, and must never run against a real data
volume.
