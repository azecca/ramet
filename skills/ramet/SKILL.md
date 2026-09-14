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

## Addresses that need the env's port

An address with another host name than `localhost` is never rewritten: a URL
a browser opens through a reverse proxy (`http://app.test`), an OAuth issuer
or redirect URI, a CORS origin. In an env where the proxy's port 80 is
published on 21000, it must read `http://app.test:21000`. Symptoms when it
does not: redirects to the main env, CORS errors, a login that fails, a blank
page. Never hard-code an env's port in a file: the same files serve every env,
and the port differs in each.

The project gives the port a name in `.ramet.json`, and the compose files use
its variable. `"ports": {"web": "proxy:80"}` names the published port
`service:container_port` shown by `ramet ls` and gives `RAMET_PORT_WEB`
(`variables` in `published`). It is set in every env `ramet new` creates, and
unset in the main env and without ramet:

```yaml
services:
  app:
    environment:
      # a colon and the port where the variable is set, nothing elsewhere
      APP_URL: http://app.test${RAMET_PORT_WEB:+:$RAMET_PORT_WEB}
      DB_PORT: ${RAMET_PORT_DB:-5432}   # a bare port: the project's port as default
```

- Write the variable only where compose replaces it: the compose files
  (`docker-compose.override.yml` included) and the `.env` compose reads. An
  `env_file`, or a `.env` the application loads itself (dotenv, Vite), is
  passed as it is: set the value in the service's `environment` instead,
  which wins over both.
- Add the port only to what a browser sees. Addresses between containers keep
  the service name (`http://keycloak:8080`); a host name the application
  compares with the `Host` header, port removed, and Traefik `Host()` rules
  stay without a port.
- A proxy reading the docker socket (Traefik) sees every env's containers and
  answers 504 for part of the requests. Keep it to its own project; `command`
  replaces the whole list, so repeat its other options:

  ```yaml
  - "--providers.docker.constraints=Label(`com.docker.compose.project`,`${COMPOSE_PROJECT_NAME}`)"
  ```

- `.ramet.json` must be in each env's worktree: committed and merged into the
  branch, or kept out of git so that `ramet sync` copies it. A file added to
  git but not committed reaches neither.
- Containers read their environment when created: `ramet compose up -d` after
  a change. A named port that is not published is reported on standard error.

`.ramet.json` and the compose files belong to the project: propose the change
and let the user decide before editing committed ones. Some fixes are the
user's alone, in the browser: each env is another origin, port included, for
Chrome's `unsafely-treat-insecure-origin-as-secure` flag (needed by the Web
Crypto API over plain HTTP, as keycloak-js with PKCE does) and for OAuth
redirect URIs stored in the env's copy of the database; cookies ignore the
port, so envs sign each other out in the same browser.

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
- Commands that change a project run one at a time: another one waits,
  printing `waiting for another ramet command on project …` on standard
  error. Wait for it rather than kill it.

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
  image `/var/lib/ramet/data.img` by hand: ramet mounts and manages them.
- `ramet df` shows how full the data volume is. When it runs low, report it
  rather than delete envs.

## When something fails

Errors go to standard error: `error:`, then usually a `hint:` naming the
command to run next. Follow the hint before improvising. Exit codes: 0 done,
1 failed or declined, 130 interrupted. `ramet doctor` diagnoses the
installation without changing anything; `ramet <command> --help` describes
every option.
