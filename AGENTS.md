# Working on ramet

Notes for AI coding agents contributing to this repository. `README.md`
describes what ramet does and how it is laid out; `tests/README.md` describes
the tests. Using ramet in other projects is covered by `skills/ramet/SKILL.md`.

## Never run ramet on the host

Only `ramet doctor` and `ramet prompt` are safe to run on the development
machine: they never mount anything. Every other command, through `cargo run`
or a built binary, mounts the machine's real data volume on `/srv/ramet` and
acts on the envs it holds. The reference scenario deletes a whole project in
it.

- `cargo test` is safe: every fixture uses a throwaway data root.
- Check real btrfs and docker behaviour in the disposable bench only:
  `tests/lab.sh up`, `tests/lab.sh run [--upto N]`, `tests/lab.sh down`.
  It needs the `x86_64-unknown-linux-musl` rustup target.
- `./install.sh`, `ramet setup` and anything under `sudo` on the host are for
  the maintainer to run. The bench is privileged: keep commands in it from
  touching the host's loop devices (no `losetup -D`).
- Reading the host is fine: `findmnt`, `stat`, `du`, sysfs.

## Before handing back

All four must pass:

```sh
cargo test
CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets   # pedantic, zero warnings
cargo fmt --check
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

A change to what ramet does to btrfs, docker or git also goes through
`tests/lab.sh run`.

## Design rules

- Commands never reach the process environment directly. The working
  directory, the terminal, external programs, btrfs and host queries come from
  the `Context`, so that integration tests run the whole command against
  doubles (`tests/integration/support`).
- Every external command goes through `process`. Subvolumes are deleted only
  through `ctx.subvolumes()`, which deletes nothing but a subvolume directly
  in the project's data directory.
- Every anticipated failure is a variant of `Error`, with a message and a
  hint naming what to do next.
- `ramet setup` is the only command that asks for privilege: it shows the
  root steps, then runs them through sudo. It never edits an existing fstab
  line.
- Each command lives in `src/commands/`, with its `Args` and a `run`
  function, and gets integration tests in `tests/integration/<command>.rs`.
  A new command also joins the samples of
  `command_names_match_what_the_user_typed` in `src/cli.rs`.

## Conventions

- Code, comments, messages and documentation are in English.
  `tests/scenario.sh` and `tests/lab.sh` are in French, for historical
  reasons.
- Comments explain why, not what. Match the tone and density of the
  surrounding code.
- Dependencies: clap, serde and serde_json, sha2, rustix, signal-hook,
  thiserror, and tempfile for tests. Ask before adding one.
- Implement what was asked; propose further features rather than add them.
- A change to the command line updates `README.md` and, when it changes how
  an agent should use ramet, `skills/ramet/SKILL.md`.
- Do not commit unless asked.
