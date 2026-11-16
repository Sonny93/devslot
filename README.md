# devslot

Run several instances of the same dev stack on one machine: each checkout or worktree gets its own block of ports and its own compose project. `ds` is the command line; a Claude Code plugin gives each agent session its own worktree, registers it and keeps agents away from other instances' data.

## Status

v1 is in development and nothing is released.

## Why

**Purpose.** Every checkout of a project publishes the same ports (Adonis 3333, Postgres 5432, Vite HMR 24678) and shares the same compose project name. Two worktrees, or two coding agents working in parallel, then share containers and volumes: a `migration:fresh` in one wipes the other's database. devslot gives each checkout a stable slot of ports and a compose project named after its path, writes the values where Compose and the app already read them, and keeps a machine-wide registry so two projects never collide.

**Users.** A developer running several checkouts of Docker Compose based projects on one Linux machine, and the coding agents (Claude Code and others) working in those checkouts at the same time. The app itself runs outside Docker.

**Stakes.**
- Data isolation: a mistake lets one instance wipe or read another instance's database.
- Secrets: devslot reads `.env` files that hold credentials, and its output is read by agents and can end up in transcripts or logs. No `.env` value is ever printed.
- Untrusted checkouts: `ds doctor` and the automatic registration hook run on repos the user may not have reviewed (fresh clones, agent sessions). Reading a repo must never run code from it.
- Other work on the machine: devslot never stops or removes containers, volumes or compose projects it did not create for the current checkout. Removing a worktree (`ds worktree rm`, the `WorktreeRemove` hook) acts on that worktree's instance, stops only its processes, and refuses while another session or program uses the worktree.

**Non-goals.**
- Production or remote deployment: devslot is a dev tool.
- Services shared between instances, background processes, named URLs and the Tunneler integration (planned after v1).
- Remapping ports written as literals in a compose file: they are reported, not managed.
- Windows and macOS: the code stays portable, nothing is tested there.

**Hard constraints.**
- A repo works the same with or without devslot: after a clone or a `ds rm`, the default ports work.
- The only write in a project is a delimited block at the end of `.env` (plus creating `.env` when it is missing), announced with the file path. devslot refuses to write into a `.env` that git does not ignore. The worktree hooks and `ds worktree rm` also create git worktrees (in the worktrees folder) and remove linked worktrees and their branches; a branch is deleted only when it is named like the worktree folder and merged.
- One change to a project is recommended, never required: writing published ports as `${VAR:-default}`.
- Integrations (Tunneler, proxies, process managers) are optional.


## Install

Requires Linux, Docker with the Compose plugin, git and rustup (the toolchain is pinned in `rust-toolchain.toml`). From a clone of this repository:

```sh
cargo install --path . --locked
```

Run the same command again after updating the clone: the hooks run the installed `ds`. `ds` must be on your `PATH`: the plugin hooks call it by name. Then, in Claude Code:

```
/plugin marketplace add <path to the clone, or Sonny93/devslot>
/plugin install devslot@devslot
```

The plugin adds a `devslot` skill and four hooks (`WorktreeCreate`, `WorktreeRemove`, `PreToolUse` on `Bash`, `SessionStart`). The `WorktreeCreate` hook replaces Claude Code's own worktree creation, and sessions started with `claude --worktree` need extra setup: see [Worktrees](docs/worktrees.md).

## Quick start

In a project that has a compose file for development:

```sh
ds doctor    # review the project: what ds manages, what would break isolation
ds up        # register this checkout, write the .env block, start the services (--no-run: skip the run key)
ds info      # name, slot, ports and URLs of this instance (--json for scripts)
ds rm        # delete the instance's data, remove the block, opt the checkout out
```

## How it works

**Slots.** The machine has a range of ports (20000 to 29999 by default) cut into slots of 50. A checkout that is registered owns one slot: each port variable ds manages (`PORT` when `.env` or `.env.example` defines it, the ports the compose file publishes, `VITE_HMR_PORT` when Vite is detected, and the variables of `[ports] extra`) gets a port inside it. The registry in `~/.local/state/devslot/registry.toml` keeps every instance of the machine, so two projects never collide.

**The `.env` block.** ds copies the values of the instance into a delimited block at the end of `.env`, so Compose, your app and your tests all read them, with or without `ds`:

```
# >>> devslot: generated automatically, do not edit by hand >>>
# To go back to the default ports: ds rm
# (also deletes the containers and data of this checkout)
PORT=20050
VITE_HMR_PORT=20051
DB_PORT=20052
COMPOSE_PROJECT_NAME=my-links-feat-auth-3f2a
DEVSLOT_INSTANCE=my-links-feat-auth-3f2a
DEVSLOT_SLOT=1
# <<< devslot <<<
```

`COMPOSE_PROJECT_NAME` gives the instance its own containers, volumes and networks. ds only writes in a `.env` that git ignores, and creates it from the main checkout's `.env` (or `.env.example`) when a new worktree has none. `[write] enabled = false` turns every write off.

**Worktrees.** With the plugin, each Claude Code session gets its own worktree and instance, and the hooks register, protect and remove them: see [Worktrees](docs/worktrees.md).

**Opt-out.** `ds rm` records the checkout as opted out: the hooks leave it alone until an explicit `ds up` or `ds run` registers it again.

To configure a project, see [Configuration](docs/configuration.md) and [Make a project work with devslot](docs/project-setup.md). The commands are listed in [Commands](docs/commands.md). Exit codes: 0 success, 1 a refusal or an error in the project, 2 the tool itself failed or invalid usage; `ds run`, and `ds up` with a `run` key, exit with the code of the command they run.

## Rules for agents

The plugin's skill gives Claude Code these rules. For other agents (Codex, Cursor and others), paste this block into an `AGENTS.md`:

```markdown
## devslot

This repository runs several instances of its dev stack on one machine, one per checkout, through devslot (`ds`). Other agents work on the same machine.

- Start the services with `ds up --no-run`. Plain `ds up` also runs the `run` key in the foreground and does not return: run it as a background command, only to get the app too. Run a command with the instance environment with `ds run -- <command>`.
- Read ports and URLs with `ds info --json`. Never guess or hardcode them: 3333, 5432 and 24678 are the defaults of other instances.
- Reset the data with `ds reset` (everything) or `ds reset <service>`.
- Never run `ds rm`, `ds prune`, `ds worktree rm`, `docker compose down -v`, `docker volume rm` or remove containers by hand: it can delete another agent's data. Ask the user.
- When the devslot worktree guard is on (the session start says so), the main checkout is for reading and searching only: before editing files or running `ds up`, `ds run` or `ds reset`, enter a worktree with the EnterWorktree tool, with a short name taken from the task, and never work around a refusal through Bash. Run `ds worktree land` only after the user's explicit go-ahead on the committed work: it is fast-forward only, so when the base moved, run `git rebase <base>` in the worktree, resolve the conflicts, commit, and run `ds worktree land` again. Never push.
- If `ds info` says this checkout is opted out, do not run `ds up` without asking the user.
- `DEVSLOT_INSTANCE` names this instance; show it in logs or on a health endpoint when useful.
- `ds doctor` explains why a checkout cannot be isolated.
```

## Known limitations

- **Two Adonis servers started in the same second can share HMR port 24678.** `@adonisjs/assembler` 8 picks the HMR port itself (24678 when free, else a random free port) and ignores `VITE_HMR_PORT` from `.env`, so the slot value is not used. Started one after the other, instances get distinct ports. Workaround: start them a few seconds apart.
- **A refused worktree removal is silent in Claude Code.** When the `WorktreeRemove` hook refuses (another session uses the worktree, or another creation or removal of it is running), Claude Code exits without showing the hook's message: the worktree stays. `ds ls` lists these worktrees with the reason; remove them with `ds worktree rm <name>`.
- **Sessions started with `claude -p` keep their worktree.** Claude Code never fires `WorktreeRemove` for them, so the worktree, its instance and its data stay until you run `ds worktree rm <name>`.
- **Sessions started with `--tmux` skip the plugin's `WorktreeCreate` hook.** See [Worktrees](docs/worktrees.md#sessions-started-with-claude---worktree-name---tmux) for the hooks to declare yourself.
- **A server started without `ds run` may survive a worktree removal.** Its process listening on an instance port is stopped, but a parent that respawns it (for example `node ace serve` run by hand) is only listed. Servers started through `ds run` or `ds up` are stopped entirely.
- **Cookies are not isolated by port.** Two instances opened on `localhost` with the same session cookie name log each other out. Named URLs per instance (after v1) will fix it.
- **Dev and local prod share the compose project** of a registered checkout, so `just prod` runs on the dev database volume. Workaround: `-p <name>` in the prod recipe (see the [spec](docs/spec.md)).
- **A session that only `cd`'d into a worktree is seen once it ran `ds up`, `ds run` or `ds reset` there.** Containers started by hand (`docker compose up`) and agents other than Claude Code stay invisible: another session's removal can delete their data. The lease lasts as long as the session's `claude` process (`/clear` and subagents included), so `ds worktree rm` from a terminal needs `--force` while it runs. See [Worktrees in use](docs/worktrees.md#worktrees-in-use-linux-only).
- **Processes are stopped on Linux only.** Elsewhere, a worktree removal stops nothing and lists nothing, and it cannot tell that another session uses the worktree.

## Develop

The checks CI runs (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)):

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

## Documentation

- [Commands](docs/commands.md): reference of every `ds` command
- [Configuration](docs/configuration.md): `devslot.toml`, `devslot.local.toml` and the global config
- [Make a project work with devslot](docs/project-setup.md): compose rules, `env_file`, Vite, `DEVSLOT_INSTANCE`
- [Worktrees](docs/worktrees.md): creation, lifecycle, worktrees in use, stopped processes
- [Specification](docs/spec.md)
- [Decisions](docs/decisions.md)
