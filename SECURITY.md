# Security

## Reporting a vulnerability

Please report vulnerabilities privately, through
[GitHub's private vulnerability reporting](https://github.com/azecca/ramet/security/advisories/new),
not in a public issue. Say what you found, how to reproduce it, and which
version of ramet you ran (`ramet doctor` prints it).

You should get an answer within a week. Once a fix is released, the advisory
is published with credit to you, unless you prefer otherwise.

Only the latest release receives security fixes.

## What counts

ramet runs `docker compose` on the compose files of a project, and running a
compose file means running what it describes: a container that damages the
host through a bind mount or a privileged option is out of ramet's reach, as
it is out of docker compose's.

What ramet does on its own is in scope, in particular:

- reading, writing or deleting a file outside the worktrees and the data
  volume ramet manages, for instance when a branch or a container plants a
  symbolic link;
- deleting data a command did not announce, such as a worktree or a
  subvolume that belongs to another env;
- gaining privileges through `ramet setup`, the one command that runs `sudo`;
- another local user reaching the data volume through what ramet creates.
