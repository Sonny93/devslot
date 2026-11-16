# Commands

Reference of the `ds` commands. `ds <command> --help` lists the options and the exit codes of each.

| Command | What it does |
| --- | --- |
| `ds doctor [--json]` | Reviews the project without starting or writing anything. |
| `ds up` | Registers the checkout, writes the block, starts the services, then runs the `run` key (`--no-run`: services only). |
| `ds down` | Stops the services and keeps the data. |
| `ds run -- <command>` | Registers the checkout if needed and runs a command with the instance environment. |
| `ds env [--format sh\|json]` | Prints the environment of the instance. |
| `ds info [--json]` | Shows the name, slot, ports, URLs and services state of this checkout's instance. |
| `ds ls [--json]` | Lists every instance of the machine with its ports and whether its services run, then the worktrees Claude Code asked to remove that are still there, with the reason. On a narrow terminal the PATH column wraps: `--json` gives the same list without it. |
| `ds ensure` | Registers the checkout and writes its block without starting anything (what the hook runs). |
| `ds reset [service]` | Deletes the data of the instance, or of one service, and starts the services again. |
| `ds rm [--yes]` | Deletes the instance's data, removes the block and opts the checkout out. |
| `ds prune [--yes]` | Deletes the data of the instances whose folder is gone and forgets them. |
| `ds worktree rm <name\|path> [--yes] [--force]` | Shows what would be lost, refuses a worktree in use by another session or program unless `--force`, then stops the instance's processes, removes a linked worktree, deletes its data, forgets its instance and deletes its branch when that is merged. |
| `ds hook <event>` | Called by the Claude Code plugin: `worktree-create`, `worktree-remove`, `pre-tool-use`, `session-start`. Not meant to be run by hand. |

**`ds worktree rm <name|path>`.** A name (letters, digits, `.`, `_`, `-`) is the worktree `<[worktrees] root>/<repo>/<name>` of the repository of the current folder; an argument that contains `/` or starts with `.` is a path, and any linked worktree root will do (never the main checkout or a subfolder). Before acting it prints the path, the branch, the instance (or "not registered"), the processes it would stop and the other processes found there, the number of uncommitted changes, and the number of commits found nowhere else (reachable from the worktree's `HEAD` only; the branch counts as a place only when it is kept). Ignored files (`.env`, local databases, uploads) are deleted with the folder, and the preview says so. It then asks like `ds rm` (a terminal, or `--yes`). A worktree with uncommitted changes or commits found nowhere else, or in use by another session or program (the preview shows an `In use` line, see [Worktrees in use](worktrees.md#worktrees-in-use-linux-only)), also needs `--force`; the changes and commits are counted again after the processes are stopped, right before `git worktree remove`, and the removal is refused (nothing deleted) when either count grew. When another creation or removal of the same worktree is running, it waits up to 10 s after the confirmation, then refuses; it also refuses when the worktree changed since the preview. A locked worktree is refused (`git worktree unlock` first) before anything is stopped, deleted or forgotten. After the data is deleted and the instance forgotten, the branch is deleted when it is named like the worktree folder and `git merge-base --is-ancestor` confirms it is merged into the `HEAD` of the main checkout; otherwise it is kept and ds says so. The hook runs the same removal without the preview and the gates, because Claude Code decides when a session ends, except for the use check below, which it applies without any way to skip it.

Exit codes: 0 success, 1 a refusal or an error in the project, 2 the tool itself failed or invalid usage. `ds run`, and `ds up` with a `run` key, exit with the code of the command they run (126 when it cannot be executed, 127 when it is not found). `--help` of each command lists its codes; `ds --help` also lists the global `--color` option and the environment variables ds reads (`NO_COLOR`, `DEVSLOT_DOCKER`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`). `ds ls --json` and `ds info --json` are versioned contracts.
