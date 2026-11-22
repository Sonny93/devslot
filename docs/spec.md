# Specification

Reference of what `ds` does. The user-facing how-to is in the [README](../README.md), [commands](commands.md), [worktrees](worktrees.md), [configuration](configuration.md) and [project setup](project-setup.md) pages. The reasons behind each rule are in [decisions](decisions.md).

## Problem and goals

The tool runs several instances of the same stack on one machine (several projects, or several worktrees of one project) without port or compose project conflicts.

Without it, every checkout publishes the same ports (an app on 3333, Postgres on 5432, Vite HMR on 24678) and shares the same compose project name. Two agents in two worktrees step on each other: same containers, same volume, and a `migration:fresh` wipes the other one's database.

Goals:

- Each checkout gets stable, automatically assigned ports and its own compose project.
- Each instance has its own services and its own data. Nothing is shared between instances.
- Once the checkout is registered, the services and the app start with the right values, whether or not you go through `ds`.
- Registration happens on its own for agents, through the hooks of the Claude Code plugin.
- `ds rm` brings the checkout back to its base configuration: default ports, nothing written by the tool.
- Agents know how to use the tool through a skill shipped with the plugin.
- All instances on the machine can be listed, as text and as JSON.

## Constraints

1. **A repo works the same with or without the tool.** After a clone, or after a `ds rm`, the project's usual command works with the default ports.
2. **A single change to the project, recommended and not required.** For a service port to be managed, the compose file writes it as `${VAR:-default}`. A hardcoded port stays as it is, with a warning.
3. **The app runs outside Docker.**
4. **A single write in the project**: the delimited block of `.env`, plus the creation of `.env` itself when it is missing. Every write is announced in the output, with the file path.
5. **Integrations are optional.**

Recommended syntax in dev compose files: `127.0.0.1:${DB_PORT:-5432}:5432`. With the default value, the compose file still works on its own.

Out of scope: services shared between instances, background processes, named URLs through a proxy (a host per instance, `<name>.localhost`, exists since 2026-11-18, opt-in through `[web] port`: see [decisions](decisions.md)), remapping of hardcoded ports, Windows and macOS (the code stays portable, but nothing is tested on them).

## Concepts

- **Checkout**: a folder that holds a working copy of the repo, the main clone or a worktree.
- **Instance**: a checkout registered in the registry, identified by its canonical path. This is the git root of the current folder (`git rev-parse --show-toplevel`, distinct for each worktree), or the current folder outside git.
- **Instance name**: `<repo>-<folder>-<hash>`, where the folder loses the repo name as a prefix (`~/dev/my-links` gives `my-links-3f2a`, `~/dev/my-links.feat-auth` gives `my-links-feat-auth-3f2a`). The repo name comes from the folder of `git rev-parse --git-common-dir`. The name is lowercased and reduced to the characters Compose accepts (`a-z`, `0-9`, `-`, `_`). The hash is the first 4 hex characters of the SHA-256 of the canonical path, lengthened when the name is already used by another path. The name is stored at registration and never recomputed. It is used as `COMPOSE_PROJECT_NAME`.
- **Slot**: a block of consecutive ports. Slot N covers `first_port + size×N` to `+size-1`. An instance has a single slot. Slot 0 is reserved and never assigned.
- **Registry**: a machine-wide file that keeps, for each instance, its path, name, slot, range and the port assigned to each variable.

The compose project name depends on the path, not on the slot, so a slot released and given to another worktree never picks up the old one's volumes.

## Commands

All commands act on the instance of the current folder, except `ls` and `prune`, which cover the machine. Only `up`, `run`, `ensure` and the `worktree-create` hook register a checkout, assign ports and write the `.env` block. `doctor`, `env`, `info` and `ls` write nothing; `down`, `reset`, `rm`, `prune` and `worktree rm` change the Docker state or the registry; `worktree land` changes only git (it fast-forwards the main checkout's branch). The effect of each command is in [commands](commands.md) and in `ds <command> --help`, which also lists the exit codes.

`ds` is the program name; every command takes `--color auto|always|never`. `up`, `run` and `ensure` refuse to register when a check finds an error. `ds rm` and `ds prune` ask for confirmation only when stdin and stderr are terminals, and change nothing without a terminal or `--yes`. `ds up` runs `docker compose up -d --wait --wait-timeout 120`, then the `run` key of `devslot.toml` through `/bin/sh -c` unless `--no-run`. `ds worktree rm <name|path>` takes `--yes` and `--force`; `ds worktree land` takes `--onto <branch>`; `ds hook` is run by the plugin, never by hand.

Exit codes: 0 success, 1 a refusal or an error in the project, 2 the tool itself failed or a usage error. `ds doctor` follows the grep and diff convention: 0 without error (warnings allowed), 1 with at least one error, 2 when the tool fails.

**The `run` key only launches the app.** `ds` manages the compose services, so the key must not run `docker compose up` again. To keep your usual command, use `ds run -- <command>` or run it directly once the block is written.

Deleting an instance's data (`ds rm`, `ds reset` without a service, `ds prune`, the worktree removal) never goes through a compose model: containers, volumes and networks are listed by the label `com.docker.compose.project=<name from the registry>` and removed by id or name.

## Port discovery

The tool finds the port variables by reading the compose file and the `.env`.

1. **Compose file**: `[compose] file`, otherwise the first that exists among `dev.compose.yaml`, `dev.compose.yml`, `compose.dev.yaml`, `compose.dev.yml`, `docker-compose.dev.yaml`, `docker-compose.dev.yml`, then the names Docker takes by default (`compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`). `ds` passes `-f <chosen file>` (plus the project's `compose.override.*` if present) to the compose commands it runs, and sets `COMPOSE_FILE` in the environment of the processes it launches. It writes `COMPOSE_FILE` into the `.env` block only when no compose file with a Docker default name exists at the checkout root, so a bare `docker compose` run there targets the same file. The block value holds paths relative to the root, joined by `:`.
2. **Service ports, by the sentinel method**:
   1. `docker compose config --variables` lists the variables of the model.
   2. `docker compose config --format json` runs with interpolation, giving each candidate variable a unique sentinel value (a port outside the slot range). A variable is a candidate when its usual value (the compose default, else its value in `.env` without the devslot block, else `.env.example`) is a number or empty.
   3. Each `published` field that contains a sentinel yields a managed variable, with its service and container port.
   The probe runs with a cleared environment, `--env-file /dev/null`, `-p devslot-probe` and a timeout, and the output never relays Compose's stderr or a resolved value.
3. **Hardcoded ports** (`"5432:5432"`) give a warning that suggests `${VAR:-5432}`. They are not managed. Port ranges are not managed either.
4. **App port**: `PORT`, read from `.env`, otherwise `.env.example`, plus the names in `[ports] extra`. Other `*_PORT` variables of the `.env` are left alone: they may point to an external server.
5. **Vite HMR**: `VITE_HMR_PORT` is added when Vite is detected: a recursive walk from the checkout root that honors `.gitignore` (the `ignore` crate) and skips `node_modules` and hidden directories, looking for a `package.json` that lists `vite` and for `vite.config.*`. The variable is allocated even for a standalone dev server, because a custom server may read it outside `vite.config`. Vite uses the HMR port 24678 only in middleware mode (`middlewareMode`); a standalone dev server serves HMR on its own `server.port`. For a middleware-mode config, which has no environment variable for its HMR port, the config reads `VITE_HMR_PORT` with one line (`server.ws.port` from Vite 8, where `server.hmr.port` is deprecated; `server.hmr.port` before). Projects that use `@adonisjs/assembler` need nothing, since it sets the variable itself.
6. **Reserved and unmanaged names**: a managed port variable name ends in `PORT` (case-insensitive) when it comes from the compose file or from `[ports] extra` of the committed `devslot.toml`. `PATH`, `HOME`, `PWD`, `SHELL`, `USER`, `TMPDIR` and names starting with `LD_`, `DOCKER_`, `COMPOSE_`, `BUILDX_`, `XDG_` or `DEVSLOT_` are never managed.
7. **When one variable has several sources**, the first wins, in this order: `PORT`, `[ports] extra`, `VITE_HMR_PORT`, compose; the compose service and container port are kept when the compose also publishes it.

Example, for an instance in slot 1 of a project with an app, Vite and Postgres:

| Variable | Source | Default | Assigned |
| --- | --- | --- | --- |
| `PORT` | `.env` | 3333 | 20050 |
| `VITE_HMR_PORT` | `package.json` (vite) | 24678 | 20051 |
| `DB_PORT` | compose, service `postgres` | 5432 | 20052 |

## Checks

The same checks serve `ds up`, `ds run`, `ds ensure` and `ds doctor`. An error blocks the command; a warning is displayed without blocking. `ds up` checks the compose it runs; `ds doctor` reviews all the compose files at the root (the 10 detection names plus `*.compose.y(a)ml`, `compose.*.y(a)ml` and `docker-compose.*.y(a)ml`) and also works on an unregistered checkout. Before `up` and the restart of `ds reset`, the isolation checks also run on the model resolved as `up` resolves it. `--json` gives the same list, with the file, service and key at fault.

| Check | Level | Why |
| --- | --- | --- |
| `container_name`: `container_name` on a service | error | Two instances would want the same container name. |
| `explicit_name`: volume or network with an explicit `name:` that does not follow the project, or `external: true` | error | It would be shared between instances. |
| `env_missing`: no `.env`, and neither the main checkout's `.env` nor `.env.example` to create it from | error | `ds` has nowhere to write its block. |
| `env_not_ignored`: `.env` not ignored by git | error | The block would end up in a commit. |
| `env_unparsable`, `env_unreadable`: `.env` that cannot be parsed (line number only), or is not a regular file or exceeds 1 MiB | error | The line can hold credentials; a FIFO or a huge file would hang or exhaust the tool. |
| `env_block_malformed`, `env_block_unwritable`: start marker without end marker, more than one block, or a value the block cannot hold (quote, `$`, `\`, line break) | error | Writing would delete user lines or produce a block that dotenv parsers read differently. |
| `compose_unreadable`: the selected dev compose is rejected by Compose | error | Nothing can be probed. |
| `compose_file_missing`: `[compose] file` names a file that does not exist | error | The configured file is not usable. |
| `env_forces_port`: committed `devslot.toml` sets a managed port variable in `[env]` | error | Only `devslot.local.toml` may force a port. |
| `env_key_undeclared`: committed `[env]` key that `.env` and `.env.example` do not declare | error | The committed file may only rebuild values the project already declares. |
| `local_config_tracked`: `devslot.local.toml` tracked by git | error | A machine-local file must stay local. |
| `forced_port_conflict`: a forced port belongs to another instance | error | Two instances would listen on the same port. |
| `forced_port_shared`: another instance forces a port that lies in this instance's slot or that this checkout forces too (`ds doctor` only) | warning | The instances clash if they run together, or this one may be handed that port later. |
| `hardcoded_port`: published port written as a literal (`"5432:5432"`) | warning | Not managed, conflicts with a second instance. |
| `port_range`: published port range | warning | Not managed. |
| `port_variable_unmanaged`: compose port published through a variable whose name does not end in `PORT` | warning | Not managed; rename it to `*_PORT`. |
| `env_file_ports`: a service loads `.env` with `env_file` and receives a managed port variable without pinning it in `environment:` | warning | The container would receive a host-side port. |
| `bind_mount`: bind mount whose resolved source is outside the checkout root | warning | The instances would share the same files. The finding names the service and the position in `volumes`, never the path. |
| `vite_hmr_port`: a Vite config that mentions `middlewareMode` and does not read `VITE_HMR_PORT` | warning | In middleware mode Vite uses HMR port 24678, so all instances would fight over it. A standalone dev server serves HMR on its own `server.port`: no warning. Skipped when `@adonisjs/assembler` is a dependency. |
| `vite_multiple_configs`: two or more Vite configs that read `VITE_HMR_PORT` or mention `middlewareMode` | warning | A single `VITE_HMR_PORT` would be shared. |
| `vite_literal_port`: a literal port in a `vite.config.*` (`port: 3333`, `const X_PORT = 3333`, a `localhost:3334` proxy target), in a `--port <n>` of a `package.json` script of a Vite manifest, or in a root justfile line that runs vite | warning | Every instance would use that port. A config that names a managed variable other than `VITE_HMR_PORT` is skipped. The finding says which variable to read, or to add to `[ports] extra`. |
| `env_test_redefines`: `.env.test` redefines a managed variable | warning | Tests would target other ports than the app. |
| `env_local_host_port`: a value of `.env` or `.env.example` holds a local host (`localhost`, `127.0.0.1`, `0.0.0.0`, `[::1]`) followed by the default port of a managed variable | warning | It does not follow the assigned port, unless `[env]` rebuilds it. External hosts are ignored. |
| `env_unmanaged_local_port`: a value of `.env` or `.env.example` holds a local host followed by a literal port that no devslot variable manages (not the port of a `[ports] ignore` variable, not an `[env]` key) | warning | The service would be shared or collide between instances. One finding per key, naming the key, the file and the port; it suggests a `*_PORT` in `[ports] extra` and `.env.example`, and rebuilding the key in `[env]`. |
| `web_port_unmanaged`: `[web] port` names a variable that is not a managed web port (unknown, or a service-kind port) | warning | Every url stays on `localhost` and `ds info --json` has no `app_url`. |
| `compose_skipped`: a scanned compose file that Compose rejects on its own | warning | Override files and fragments fail alone; they are merged into the dev compose instead. |
| `slot_geometry_changed`: the registered range no longer matches the slot settings | warning | The instance keeps its ports; a change of slot settings does not move it. |
| `session_process_name`: `CLAUDE_PID` is set (a Claude Code session) and the session process is not named `claude`, or none is found (Linux only) | warning | No session lease is written, so a worktree removal by another session will not see this session's worktrees in use. |
| `tunneler_host_invalid`: `[tunneler] host` in the global config is empty, longer than 64 characters, has a character outside `[A-Za-z0-9._-]`, does not start with a letter or a digit, or is not a string | error | Only `ds doctor` fails on it: `ds info` and the SessionStart context say it and leave the Tunneler hint out, every other command ignores it. |
| `worktree_require_ignored`: `devslot.toml`, or a tracked `devslot.local.toml`, sets `[worktrees] require` | warning | Only an untracked `devslot.local.toml` can switch the worktree guard off for a checkout. |
| `worktree_guard`: `[worktrees] require` is set | info | Says whether the guard is active, covers a linked worktree, is switched off locally, or does not cover the checkout. |
| `worktree_guard_registered`: a covered main checkout is registered | info | `ds ensure` leaves its slot and `.env` block as they are and no longer updates them. |
| `compose_missing`: no compose file at the root | info | There is nothing to isolate; the other checks still run. |

**Rule for `env_file`**: block values are host-side ports, so a container that loads `.env` pins its internal ports in `environment:`, which takes precedence over `env_file`:

```yaml
  app:
    env_file:
      - .env
    environment:
      - DB_HOST=postgres
      - DB_PORT=5432
      - PORT=3333
```

**Known limitations.** Dev and a local prod compose share the compose project, so local prod runs on the dev database volume of that checkout (workaround: `-p <name>` in the prod recipe, which takes precedence over `COMPOSE_PROJECT_NAME`). Cookies are not isolated by port: two instances opened on `localhost` with the same cookie name log each other out. An `-f` on the command line takes precedence over `COMPOSE_FILE`.

## Assignment and registry

An instance keeps the same ports as long as it exists.

1. **Lock**: `flock` on `registry.lock`. Two agents started at the same time never take the same slot.
2. **Known instance**: it gets its ports back. A new variable takes the next free port of its range. A variable that has disappeared keeps its port.
3. **Full slot**: a clear error that states the number of ports requested and suggests increasing the slot size.
4. **New instance**: it takes, from slot 1, the smallest slot whose range overlaps no registered range and holds no registered port, and whose ports are all free. A port is free if a `bind` succeeds on `127.0.0.1`, `0.0.0.0`, `::1` and `::`; `EADDRNOTAVAIL` on IPv6 is ignored.
5. **Check before every `up`**: a port used by the instance's own containers is normal. Used by another program, it is an error that names the port.
6. **Atomic write**: temporary file then `rename`, before releasing the lock. A malformed registry (overlapping ranges, shared ports, duplicate paths or names, a port outside its range) is never rewritten.

The registry is TOML, readable by hand, in `~/.local/state/devslot/registry.toml` (`XDG_STATE_HOME`):

```toml
version = 1

[[instance]]
path = "/home/me/dev/my-links.feat-auth"
name = "my-links-feat-auth-3f2a"
slot = 1
host = "my-links-feat-auth-3f2a.localhost"
web_port = "PORT"   # only when [web] port is set
project = "my-links"
worktree = true
range = [20050, 20099]
ports = { PORT = 20050, VITE_HMR_PORT = 20051, DB_PORT = 20052 }
details = { PORT = { kind = "web" }, VITE_HMR_PORT = { kind = "web" }, DB_PORT = { kind = "service", service = "postgres", target = 5432 } }
```

`range` is the block the slot covered when assigned, so an instance keeps its ports after a change of slot size. `details` gives the kind of each port, and for a service port its compose service and container port, so `ds info` and `ds ls` need no Docker call. `host` is the instance's `DEVSLOT_HOST`, chosen at registration from the label of the instance name (`_` to `-`, at most 63 characters keeping the hash; `-<slot>`, then `-<slot>-<n>`, appended while another instance has the host) and stored: it is never recomputed. `web_port` is the variable `[web] port` named when the entry was last saved. `project` (the raw repository name) and `worktree` (whether git counts the checkout as a linked worktree) come from one git call at registration, are always written together, and are read leniently: an entry without them, with one missing, of the wrong type or with an empty `project` reads as having no origin (`null` in `ds ls --json`) and never makes the registry malformed; the next registration fills them, and so does `ds ensure` except for non-git checkouts, main checkouts under the `require` guard and checkouts with writing disabled, which wait for the next `ds up` or `ds run`. An entry without `host` (written by an older version) gets one when it is read and saved at the next registration refresh; an invalid host, or one used by two entries, makes the registry malformed. A top-level `opted_out` list holds the paths of checkouts removed by `ds rm`.

## Environment and the `.env` block

The tool provides the instance values in two ways: it passes them to the process it launches, and it copies them into a delimited block at the end of `.env`. With or without `ds`, everyone reads the same values.

**Variables**, in the order of the block: each port variable, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE` (only when needed, see Port discovery), `DEVSLOT_INSTANCE`, `DEVSLOT_SLOT`, `DEVSLOT_HOST`, then the built values of `[env]`. `DEVSLOT_INSTANCE` is the instance name, `DEVSLOT_SLOT` its slot number and `DEVSLOT_HOST` its host (`<label>.localhost`, see [Configuration](configuration.md#per-instance-host-and-web-port)), so an app can show which instance it is. Credentials, `APP_KEY` and the rest of the `.env` are never touched.

**`ds` overwrites its own variables.** If the caller's environment already defines one with another value (for example after an `eval $(ds env)` in another checkout), `ds run` replaces it and reports it on stderr. `ds env` prints only the port variables, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE`, `DEVSLOT_*` and the `[env]` values of `devslot.local.toml`. To force a value, use `devslot.local.toml`.

**Why this is enough.** Compose takes shell variables before those of `.env`, and `COMPOSE_PROJECT_NAME` takes precedence over the file's `name:`. Adonis gives `process.env` priority over its `.env` files. The block goes in `.env` because `.env.local` is read neither by Compose nor by the tests.

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
DEVSLOT_HOST=my-links-feat-auth-3f2a.localhost
# <<< devslot <<<
```

**Write rules**:

- The block is written by `ds up`, `ds run`, `ds ensure` and the `worktree-create` hook, under the registry lock, through a temporary file and `rename`. Only the content between the markers changes. A `.env` that is a symlink or not a regular file is refused. Every write is announced with the file path.
- A missing `.env` is created from the main checkout's `.env` (block stripped, permissions copied; it may be a symlink to a regular file of at most 1 MiB), otherwise from `.env.example` (a symlinked one is refused). It is an error only when neither exists.
- The block is never written into a `.env` that git does not ignore.
- A value is written bare when made of `[A-Za-z0-9_./:@,+=%-]`, single-quoted when it holds no `'`, `$`, `\` or newline, and refused otherwise.
- If a managed variable already exists higher up in the file, the original line is left untouched: Compose, dotenv and the Adonis `EnvParser` keep the last value of a duplicated key, so the block wins.
- A block deleted by hand is rewritten on the next write command. The way out is `ds rm`.
- `ds rm` removes the block, keeps a `.env` that `ds` created, and records the checkout as opted out: `ds ensure` leaves it alone until an explicit `ds up` or `ds run`. `ds prune` also forgets opted-out checkouts whose folder is gone.
- `[write] enabled = false` (global config or `devslot.toml`, the local file wins): `ds` neither creates `.env` nor writes the block, and warns when a block already exists. Commands launched by `ds run` still get the instance values.

## Plugin for agents

The repo contains a Claude Code plugin, `devslot`, installed from the marketplace of the repo (`/plugin install devslot@devslot`, see the README). Its parts:

- **A `devslot` skill**, chosen by the model from its description. Its rules: start the services with `ds up --no-run`, read ports and URLs with `ds info --json`, reset data with `ds reset`, never run `ds rm`, `ds prune`, `ds worktree rm` or delete containers or volumes by hand.
- **A `finish` skill** (`/devslot:finish`), run only by the user (`disable-model-invocation: true`): invoking it is the go-ahead to land the work with `ds worktree land` and leave with `ExitWorktree` (action `remove`, `discard_changes: true`), which stops the instance and deletes its containers and volumes.
- **A `WorktreeCreate` hook** (`ds hook worktree-create`). It replaces Claude Code's own worktree creation: `git worktree add` in `<[worktrees] root>/<repo>/<name>` (default `~/dev/.worktrees`) on branch `<name>` (reused when it exists, else created from the branch checked out in the main worktree), then registers the checkout when it is in the `[auto]` scope and has a dev compose file, and prints the path last on stdout. An existing linked worktree of the same repository at that path is reopened unless git lists it as locked, any other existing target is refused. It runs under a 170 s budget: `git worktree add` gets the remaining budget minus 20 s; an add cut short during the checkout is cleaned up (worktree, and the branch it created), one cut short during `post-checkout` is kept and its path printed; registration is skipped below 20 s left.
- **A `WorktreeRemove` hook** (`ds hook worktree-remove`). It acts only on the root of a linked worktree. It refuses (exit 1, nothing deleted, nothing stopped) when the worktree holds uncommitted changes or commits found nowhere else (counted before stopping and again before `git worktree remove`, under a 150 s budget), or when a process of another session or program runs in the worktree, stops the instance's processes, removes the git worktree, deletes the instance's data by label and forgets it, then deletes the branch when it is named like the worktree folder and merged into the main checkout's `HEAD`. `ds worktree rm` runs the same sequence with a preview and confirmation.
- **A `PreToolUse` hook** (matchers `Bash`, and `Edit|Write|MultiEdit|NotebookEdit` in a second entry with a 10 s timeout, both `ds hook pre-tool-use`; input up to 32 MiB). For a write tool it only applies the worktree guard (5 s budget, no registration), see [worktrees](worktrees.md#worktree-guard). For Bash it first denies `ds rm`, `ds prune`, `ds worktree rm` and every `ds hook` command, and `ds up`, `ds run` and `ds reset` when the `cwd` is a main checkout covered by the guard. The guard reads the text of the command, not the shell: it handles separators, wrappers (`sudo`, `env`, `timeout`, `xargs`...), paths ending in `/ds`, quotes and `ds run --`, but a command built at run time gets through, and a quoted string such as `echo "ds rm"` is denied too. It then runs the work of `ds ensure` on the `cwd` of the input (which leaves a covered main checkout alone), so a checkout entered or created mid-session is registered with no action from the agent. It always exits 0: failures become context for the agent. It runs under a 15 s budget (Claude Code kills it after 20 s); running out exits silently and silences the checkout for 15 s, then 30, 60 and 90 s on consecutive exhaustions.
- **A `SessionStart` hook** (`ds hook session-start`). When the checkout is registered, it adds the output of `ds info` and the agent rules to the context, with no Docker call. When the worktree guard is on for the checkout, it first adds a line about it. When the commit `ds` was built from does not start with the name of the plugin copy Claude Code runs (the last component of `CLAUDE_PLUGIN_ROOT`, 12 hexadecimal characters since the plugin has no version), it adds a line telling the agent to tell the user to run `scripts/install.sh`; it stays silent when either is unknown.

`ds ensure` registers a checkout only when it is in a git repository, inside the `[auto]` scope, not opted out, and has a dev compose file. A refused registration is cached by checkout path and a fingerprint of the files that matter, so a refusal is reported once.

## Configuration file

A `devslot.toml` at the repo root, optional, committed or not. A `devslot.local.toml`, ignored by git, overrides it for the machine. Priority order: `devslot.local.toml`, `devslot.toml`, the global config, then the default values. There is no command line option for configuration. [configuration](configuration.md) documents every key.

```toml
# devslot.toml: everything is optional
run = "node ace serve --hmr"   # app command run by ds up, in the foreground

[compose]
file = "dev.compose.yml"       # otherwise detected

[ports]
extra = ["FRONT_PORT"]         # variables to manage in addition to PORT
ignore = ["MAILPIT_PORT"]      # compose variables to leave as they are

[env]
VITE_API_URL = "http://localhost:${PORT}"   # built from the assigned ports

[write]
enabled = true
```

```toml
# ~/.config/devslot/config.toml (XDG_CONFIG_HOME): everything is optional
[slots]
first_port = 20000
last_port = 29999
size = 50                      # at most 100

[auto]
include = []                   # path globs; empty means everywhere
exclude = []                   # wins over include

[write]
enabled = true

[worktrees]
root = "~/dev/.worktrees"
require = []                   # path globs of the main checkouts agents must not work in; empty means off
require_exclude = []           # wins over require

[tunneler]
host = "vega"                  # SSH alias of this server on your computer; absent means off
```

`[env]` keys match `[A-Za-z_][A-Za-z0-9_]*`, values are strings or integers, and `${NAME}` is replaced by an instance variable (an unknown name is an error; `${DEVSLOT_HOST}` is the instance's host). Reserved names and a denylist of names that make programs run code are refused. The committed `devslot.toml` may only set keys already declared in `.env` or `.env.example`; a managed port may be forced only in `devslot.local.toml`. `ds info` shows where the compose file choice comes from: `devslot.toml`, `devslot.local.toml` or `detected`.

## JSON output

`ds ls --json`, `ds info --json` and `ds doctor --json` are versioned (`"version": 1`). One rule for absent values: a key is omitted when it does not apply (the per-port `service`, `target`, `url` and `forced`), and is `null` when the value is unknown or not set (`services_running`, `app_url`, `project`, `worktree`). One exception to the `null` half: `tunneler` in `ds info --json` is omitted, never `null`, when it is not configured, invalid, or the instance name is outside its bounds. Absent means the integration is off, and a `null` would make agents mention it. A client ignores the keys it does not know: a later version only adds keys.

`ds ls --json`:

```json
{
  "version": 1,
  "shared": [],
  "instances": [
    {
      "name": "my-links-feat-auth-3f2a",
      "project": "my-links",
      "path": "/home/me/dev/my-links.feat-auth",
      "worktree": true,
      "slot": 1,
      "host": "my-links-feat-auth-3f2a.localhost",
      "app_url": "http://my-links-feat-auth-3f2a.localhost:20050",
      "services_running": true,
      "ports": [
        { "var": "PORT", "port": 20050, "kind": "web", "url": "http://my-links-feat-auth-3f2a.localhost:20050" },
        { "var": "VITE_HMR_PORT", "port": 20051, "kind": "web", "url": "http://localhost:20051" },
        { "var": "DB_PORT", "port": 20052, "kind": "service", "service": "postgres", "target": 5432 }
      ]
    }
  ]
}
```

`shared` is an array of objects, always empty in v1; a client ignores keys it does not know. `project` is the raw repository name (the folder name outside git): the same for every worktree of a repository and not unique across repositories, so a client must not key on it. `worktree` is `true` for a linked git worktree, `false` for the main checkout or outside git, and `null` when the entry predates the field. Both are stored in the registry at registration, so `ds ls` runs no git. `app_url` is the one of `ds info --json`, `null` without `[web] port`. `services_running` is `null` when Docker could not tell. Only web ports have a `url`. `port` is the effective port: the one allocated in the slot, or the value `devslot.local.toml` forces for that variable when it is canonical (1 to 65535, no sign or leading zero). `"forced": true` marks a forced port, absent otherwise; it lies outside the slot `range` and may be shared with another instance, so a consumer must not key on the port alone. `ds ls` reads only the `devslot.local.toml` of each instance, and lists the allocated ports when that file is missing or malformed.

`ds info --json` of a registered checkout has `version`, `registered: true`, `name`, `project` and `worktree` (as in `ds ls --json`), `path`, `slot`, `host`, `app_url` (`http://<host>:<port>` when `[web] port` names a managed web port, `null` otherwise), `range` (`[first, last]`), `compose_file`, `compose_file_origin`, `ports` (same shape as above), `services` (`running`, `stopped`, `none` or `unknown`) and, only when `[tunneler] host` is valid in the global config, `tunneler` (`{"command": "tunneler devslot <host> <instance>", "url": "tunneler://devslot/<host>/<instance>"}`, both segments percent-encoded). `ds ls --json` has no `tunneler`. An unregistered checkout gives `version`, `registered: false`, `path` and `opted_out`.

`ds doctor --json` has `version`, `checkout`, `compose_file`, `ports` (`var`, `kind`, `default`, `source`, `file`, `service`, `target`), `findings` (`level`, `check`, `file`, `service`, `key`, `message`) and `summary` (`errors`, `warnings`).

## Tech stack

A single Rust binary named `ds` (crate `devslot`), edition 2024, clippy pedantic, no `unwrap` outside tests, unpublished.

| Need | Choice |
| --- | --- |
| CLI | `clap` (derive) |
| Reading the compose | `docker compose config --variables`, then `config --format json` with sentinel values, read with `serde_json`. No YAML parser. |
| `.env` | `dotenvy` for reading; home-made block writing between markers, through a temporary file and `rename` |
| Registry and config | `serde` + `toml_edit`, so hand-written comments survive rewrites |
| Vite detection, path globs | `ignore` and `globset` |
| Lock | `flock` on `registry.lock`, through the standard library |
| Processes and ports (Unix) | `rustix`: process scan, pidfd signals, port binding |
| Launching the app | `exec` through `/bin/sh -c`: the command replaces the tool, so Ctrl+C and the exit code behave as without it |
| Docker | Calls to the `docker compose` command, no SDK; `DEVSLOT_DOCKER` names another program |

Tests: unit tests next to the code, and integration tests in `tests/` with a fake `docker` (a script designated by `DEVSLOT_DOCKER`) so CI does not depend on a real daemon.
