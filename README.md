# devslot

Run several instances of the same dev stack on one machine: each checkout or worktree gets its own block of ports and its own compose project. `ds` is the command line; a Claude Code plugin gives each agent session its own worktree, registers it and keeps agents away from other instances' data.

## Status

v1 is in development and nothing is released. Available commands: `ds doctor`, `ds up`, `ds down`, `ds run`, `ds env`, `ds info`, `ds ls`, `ds ensure`, `ds reset`, `ds rm`, `ds prune` and `ds hook` (called by the plugin). The Tunneler integration, shared services and background processes are planned after v1.

## Why

**Purpose.** Every checkout of a project publishes the same ports (Adonis 3333, Postgres 5432, Vite HMR 24678) and shares the same compose project name. Two worktrees, or two coding agents working in parallel, then share containers and volumes: a `migration:fresh` in one wipes the other's database. devslot gives each checkout a stable slot of ports and a compose project named after its path, writes the values where Compose and the app already read them, and keeps a machine-wide registry so two projects never collide.

**Users.** A developer running several checkouts of Docker Compose based projects on one Linux machine, and the coding agents (Claude Code and others) working in those checkouts at the same time. The app itself runs outside Docker.

**Stakes.**
- Data isolation: a mistake lets one instance wipe or read another instance's database.
- Secrets: devslot reads `.env` files that hold credentials, and its output is read by agents and can end up in transcripts or logs. No `.env` value is ever printed.
- Untrusted checkouts: `ds doctor` and the automatic registration hook run on repos the user may not have reviewed (fresh clones, agent sessions). Reading a repo must never run code from it.
- Other work on the machine: devslot never stops or removes containers, volumes or compose projects it did not create for the current checkout.

**Non-goals.**
- Production or remote deployment: devslot is a dev tool.
- Services shared between instances, background processes, named URLs and the Tunneler integration (planned after v1).
- Remapping ports written as literals in a compose file: they are reported, not managed.
- Windows and macOS: the code stays portable, nothing is tested there.

**Hard constraints.**
- A repo works the same with or without devslot: after a clone or a `ds rm`, the default ports work.
- The only write in a project is a delimited block at the end of `.env` (plus creating `.env` when it is missing), announced with the file path. devslot refuses to write into a `.env` that git does not ignore.
- One change to a project is recommended, never required: writing published ports as `${VAR:-default}`.
- Integrations (Tunneler, proxies, process managers) are optional.

## Install

From a clone of this repository:

```sh
cargo install --path . --locked
```

`ds` must be on your `PATH`: the plugin hooks call it by name. Then, in Claude Code:

```
/plugin marketplace add <path to the clone, or Sonny93/devslot>
/plugin install devslot@devslot
```

The plugin adds a `devslot` skill and four hooks (`WorktreeCreate`, `WorktreeRemove`, `PreToolUse` on `Bash`, `SessionStart`).

The `WorktreeCreate` hook replaces Claude Code's own worktree creation: `ds hook worktree-create` runs `git worktree add` itself and puts the worktree in `~/dev/.worktrees/<repo>/<name>`, on a branch called `<name>` (the existing one when there is one, else a new one from the branch checked out in the main worktree). Change the folder with `[worktrees] root` in the global config (see Configuration). Without the plugin, Claude Code creates its worktrees where it always did.

## Quick start

In a project that has a compose file for development:

```sh
ds doctor    # review the project: what ds manages, what would break isolation
ds up        # register this checkout, write the .env block, start the services
ds info      # name, slot, ports and URLs of this instance (--json for scripts)
ds rm        # delete the instance's data, remove the block, opt the checkout out
```

## How it works

**Slots.** The machine has a range of ports (20000 to 29999 by default) cut into slots of 50. A checkout that is registered owns one slot: each port variable ds manages (`PORT`, the ports the compose file publishes, `VITE_HMR_PORT` when Vite is detected) gets a port inside it. The registry in `~/.local/state/devslot/registry.toml` keeps every instance of the machine, so two projects never collide.

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

**Worktree lifecycle.** With the plugin, a Claude Code session in a worktree goes like this:

1. `WorktreeCreate`: `ds hook worktree-create` creates the worktree, then registers it (slot, `.env`, block). It never starts services.
2. `PreToolUse`: before each Bash command, `ds hook pre-tool-use` registers a checkout that is not (worktrees entered mid-session included), tells the agent about its instance, and denies `ds rm` and `ds prune`. It never blocks anything else.
3. `SessionStart`: `ds hook session-start` adds the instance to the agent's context.
4. `WorktreeRemove`: `ds hook worktree-remove` deletes the instance's containers, volumes and networks by label, forgets the instance, removes the worktree, and deletes its branch only when it is merged into the main worktree's branch. When Docker fails, the registry keeps the entry and `ds prune` cleans it later.

**Opt-out.** `ds rm` records the checkout as opted out: the hooks leave it alone until an explicit `ds up` or `ds run` registers it again.

## Commands

| Command | What it does |
| --- | --- |
| `ds doctor [--json]` | Reviews the project without starting or writing anything. |
| `ds up` | Registers the checkout, writes the block, starts the services, then runs the `run` key. |
| `ds down` | Stops the services and keeps the data. |
| `ds run -- <command>` | Registers the checkout if needed and runs a command with the instance environment. |
| `ds env [--format sh\|json]` | Prints the environment of the instance. |
| `ds info [--json]` | Shows the name, slot, ports, URLs and services state of this checkout's instance. |
| `ds ls [--json]` | Lists every instance of the machine with its ports and whether its services run. |
| `ds ensure` | Registers the checkout and writes its block without starting anything (what the hook runs). |
| `ds reset [service]` | Deletes the data of the instance, or of one service, and starts the services again. |
| `ds rm [--yes]` | Deletes the instance's data, removes the block and opts the checkout out. |
| `ds prune [--yes]` | Deletes the data of the instances whose folder is gone and forgets them. |
| `ds hook <event>` | Called by the Claude Code plugin: `worktree-create`, `worktree-remove`, `pre-tool-use`, `session-start`. Not meant to be run by hand. |

Exit codes: 0 success, 1 a refusal or an error in the project, 2 the tool itself failed or invalid usage. `--help` of each command lists its codes. `ds ls --json` and `ds info --json` are versioned contracts.

## Configuration

Priority: `devslot.local.toml`, `devslot.toml`, the global config, the defaults. Everything is optional.

`devslot.toml` at the repository root (committed or not), overridden on your machine by `devslot.local.toml` (ignored by git):

```toml
# App command, run by ds up in the foreground after the services.
run = "node ace serve --hmr"

[compose]
file = "dev.compose.yml"   # otherwise detected

[ports]
extra = ["FRONT_PORT"]     # variables to manage in addition to PORT
ignore = ["MAILPIT_PORT"]  # compose variables to leave as they are

[env]
# values built from the ports of the instance, also written in the block
VITE_API_URL = "http://localhost:${PORT}"

[write]
enabled = true             # false: nothing is written to .env
```

`~/.config/devslot/config.toml`, the global config:

```toml
[slots]
first_port = 20000   # the range cut into slots; slot 0 is reserved
last_port = 29999
size = 50            # ports per slot, at most 100

[auto]
include = []         # path globs where the hooks may register; empty means everywhere
exclude = []         # wins over include, e.g. ["~/clones/**"]

[write]
enabled = true       # false: ds never writes .env, unless a project file says otherwise

[worktrees]
root = "~/dev/.worktrees"   # where ds hook worktree-create puts worktrees: <root>/<repo>/<name>
```

## Project setup

The only change devslot recommends, never requires: write the ports a compose file publishes as `${VAR:-default}`, so the default ports still work without devslot.

```yaml
services:
  postgres:
    ports:
      - '${DB_PORT:-5432}:5432'
```

`ds doctor` reports a literal port (`"5432:5432"`); ds does not remap it.

Vite has no environment variable for its HMR port. Add one line to `vite.config.ts`, which falls back to the default without devslot:

```ts
server: { hmr: { port: Number(process.env.VITE_HMR_PORT) || undefined } }
```

**`env_file` rule.** The values in the block are host-side ports. A container that loads `.env` with `env_file` must pin its internal ports in `environment:`, which wins over `env_file`:

```yaml
  app:
    env_file:
      - .env
    environment:
      - DB_HOST=postgres
      - DB_PORT=5432
      - PORT=3333
```

**`[env]` builds values from the ports.** Use it for URLs that must follow the assigned ports, and to hand `DEVSLOT_INSTANCE` to a frontend or a mobile app, which only reads variables with its own prefix. Declare the key in `.env.example` too, empty:

```toml
[env]
DATABASE_URL = "postgres://localhost:${DB_PORT}/app"
EXPO_PUBLIC_DEVSLOT_INSTANCE = "${DEVSLOT_INSTANCE}"
```

```sh
# .env.example
EXPO_PUBLIC_DEVSLOT_INSTANCE=
```

The app can then show which instance it talks to. `DEVSLOT_INSTANCE` is also in the environment of everything `ds run` and `ds up` start.

## Rules for agents

The plugin's skill gives Claude Code these rules. For other agents (Codex, Cursor and others), paste this block into an `AGENTS.md`:

```markdown
## devslot

This repository runs several instances of its dev stack on one machine, one per checkout, through devslot (`ds`). Other agents work on the same machine.

- Start the services with `ds up`. Run a command with the instance environment with `ds run -- <command>`.
- Read ports and URLs with `ds info --json`. Never guess or hardcode them: 3333, 5432 and 24678 are the defaults of other instances.
- Reset the data with `ds reset` (everything) or `ds reset <service>`.
- Never run `ds rm`, `ds prune`, `docker compose down -v`, `docker volume rm` or remove containers by hand: it can delete another agent's data. Ask the user.
- If `ds info` says this checkout is opted out, do not run `ds up` without asking the user.
- `DEVSLOT_INSTANCE` names this instance; show it in logs or on a health endpoint when useful.
- `ds doctor` explains why a checkout cannot be isolated.
```

## Develop

The checks CI runs (see [`.github/workflows/ci.yml`](.github/workflows/ci.yml)):

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

## Documentation

- [Specification](docs/spec.md)
- [Decisions](docs/decisions.md)
