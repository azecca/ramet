---
name: ramet
description: Work safely in projects managed by ramet, which gives each git branch its own worktree, docker compose stack and snapshotted data. Use when a repository's stack runs through `ramet compose`, when `ramet prompt` prints an env, or when a task calls for a disposable copy of the app and its database - parallel work, a risky migration, a data experiment to undo.
---

# ramet

ramet clones a docker compose project per git branch. `ramet new feat-x`
creates a git worktree, a copy-on-write snapshot of the current env's data
(its database included) and a stack of its own, on ports of its own.
`ramet checkpoint` sets a return point on that data, `ramet restore` goes back
to it.

## Where you are

Like git with branches, ramet acts on the env of the working directory. A
command run from the wrong worktree acts on the wrong env.

- `ramet prompt` prints `project:env`, or nothing outside an env, and changes
  nothing. Run it first, and again before anything destructive.
- `ramet ls --json` lists the project's envs: `current`, then for each env its
  `worktree`, `branch`, `state`, `published` ports and `checkpoints`.
- `ramet path <env>` prints an env's worktree. Act on another env from there:
  `cd "$(ramet path feat-x)" && ramet compose ps`.

## Running the stack

In an env, every docker compose command goes through `ramet compose`, its
arguments untouched:

```sh
ramet compose up -d
ramet compose logs --tail 100 web
ramet compose exec -T db psql -U app -c 'select 1'
```

Never run `docker compose` directly in an env: it would reach another stack,
or create one without the env's data and ports. `ramet compose` names the env
it targets on standard error (`· shop/feat-x (feat-x)`) and exits with
compose's exit code.

Each env has host ports of its own: do not assume the project's usual ones.
Read them from `published` in `ramet ls --json`. The env's `.env` files were
copied with their `localhost:<port>` rewritten to the env's ports.

## Parallel work

```sh
ramet new feat-x                  # from the current env's data, as it is now
ramet new feat-x --from c1        # from one of the current env's checkpoints
ramet new fix-y --branch fix/y    # a branch named otherwise than the env
```

- `ramet new` clones the env you stand in, starts the new stack, and prints
  its worktree (`<repo>.wt/<env>`, next to the main clone) and ports.
- A new branch starts from the current env's last commit: uncommitted changes
  stay behind. Only local files git ignores, such as `.env`, are copied.
- A clone costs almost nothing: it shares every file it has not changed.
- Names: letters, digits, `.`, `_` and `-`, starting with a letter or digit.

## Checkpoints

Take one before anything that changes data for good: a migration, a bulk
update, a destructive script.

```sh
ramet checkpoint before-migration -m "before 0042_split_users"
ramet log --json    # label, created_at, head (the commit then), exclusive_bytes
ramet restore before-migration --yes --pre-restore
```

`restore` rewinds the env's data, not its code: `head` in `ramet log --json`
says which commit the data went with. It stops the stack, swaps the data and
starts the stack again. `--pre-restore` checkpoints the current data first, so
that the restore itself can be undone.

A checkpoint freezes the stack for about 0.1 s, connections kept. `--live`
skips the freeze, at the price of a snapshot taken while the database writes.

## Destructive commands: the human decides

Without a terminal, a command that needs a confirmation refuses and changes
nothing (`there is no terminal to confirm on: rerun with --yes`). Adding
`--yes` is deciding to destroy: do it only for an operation the user asked for
or confirmed, naming what will be lost.

| command | destroys |
|---|---|
| `ramet restore <label> --yes` | the env's data written since the checkpoint |
| `ramet rm <env> --yes` | the env's worktree **with its uncommitted changes**, its data and its checkpoints; the branch stays |
| `ramet sync --yes` | the local files (`.env`…) that differ from the source env's |
| `ramet prune --yes` | the orphaned envs and checkpoints of **every project** on the machine |
| `ramet deinit --yes` | the main env's data and checkpoints; the original docker volumes stay |

Before `ramet rm`, commit or push what the worktree holds. `rm` refuses the
main env and the env you stand in: run it from another env.

## Left to the human

- `ramet setup` (it runs sudo), `ramet init`, `ramet deinit` and `ramet
  prune` administer the machine or the project; they are not part of a task.
- Never run ramet under `sudo`: it refuses. Never touch `/srv/ramet` or the
  image in `~/.local/share/ramet` by hand: ramet mounts and manages them.
- `ramet df` shows how full the data volume is. When it runs low, report it
  rather than delete envs.

## When something fails

Errors go to standard error: `error:`, then usually a `hint:` naming the
command to run next. Follow the hint before improvising. Exit codes: 0 done,
1 failed or declined, 130 interrupted. `ramet doctor` diagnoses the
installation without changing anything; `ramet <command> --help` describes
every option.
