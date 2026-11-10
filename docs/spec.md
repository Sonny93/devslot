# Spec v1: dev stack isolation tool (revision 3)

Nov 9, 2026 · @Sonny

Revision of November 9, after the review of [revision 2](https://claude.ai/artifact/7yfSVj6xwfQ4wfj2HuMMCd): a delimited block in `.env` replaces `.env.local`, `ds` always overwrites its own variables, `ds rm` brings a checkout back to its base configuration, and no open question remains for v1.

## Problem and goals

The tool runs several instances of the same stack on one machine (several projects, or several worktrees of one project) without port or compose project conflicts.

Today, every checkout publishes the same ports (Adonis 3333, Postgres 5432, Vite HMR 24678) and shares the same compose project name. Two agents in two worktrees step on each other: same containers, same volume, and `migration:fresh` wipes the other one's database.

Goals of v1:

- Each checkout gets stable, automatically assigned ports and its own compose project.
- Each instance has its own services and its own data. Nothing is shared between instances in v1.
- Once the checkout is registered, the services and the app start with the right values, whether or not you go through `ds`.
- `ds rm` brings the checkout back to its base configuration: default ports, nothing written by the tool.
- Agents know how to use the tool thanks to a Claude Code plugin shipped with it.
- All instances on the machine can be listed, as text and as JSON.

## Constraints

1. **A repo works the same with or without the tool.** After a clone, or after a `ds rm`, `just dev` works with the default ports.
2. **A single change to the project, and it is recommended, not required.** For a service port to be managed, the compose file must write it as `${VAR:-default}`. A hardcoded port stays as it is, with a warning.
3. **The app runs outside Docker**, as it does today.
4. **A single write in the project**: the delimited block of `.env`. Every write is announced in the output, with the file path.
5. **Integrations are optional.** Tunneler, a proxy or a process manager are offered, never imposed.

Recommended syntax in dev compose files: `127.0.0.1:${DB_PORT:-5432}:5432`. With the default value, the compose file still works on its own.

**Out of v1**: services shared between instances, background processes, named URLs, Tunneler integration, remapped hardcoded ports, Windows and macOS (the code stays portable, but nothing is tested on them).

## Tool name

Name chosen for v1: **devslot**, short alias `ds`, for "one dev slot per checkout". It can still change as long as nothing is published.

Free alternatives on crates.io: portlane (`pl`), berthing (`bt`), dockslot (`dk`). Already taken: berth, quay, pier, moor, slip, wharf, bollard. Checked on November 9, 2026; devslot also looks free on npm and GitHub.

## Concepts

- **Checkout**: a folder that holds a working copy of the repo, whether it is the main clone or a worktree. Three worktrees plus the main clone make four checkouts.
- **Instance**: a checkout registered in the registry, identified by its canonical path. This is the git root of the current folder (`git rev-parse --show-toplevel`, distinct for each worktree), or the current folder outside git.
- **Instance name**: `<repo>-<folder>` plus 4 characters of a hash of the path, for example `my-links-feat-auth-3f2a`. The repo name comes from the folder of `git rev-parse --git-common-dir`, the same for all worktrees. The name is lowercased and reduced to the characters Compose accepts (`a-z`, `0-9`, `-`, `_`). It is used as `COMPOSE_PROJECT_NAME`.
- **Slot**: a block of 50 consecutive ports (size adjustable in the global config, up to 100). Slot N covers `20000 + 50×N` to `+49`. An instance has a single slot. Slot 0 stays reserved for the shared servers of v2, so that v2 does not have to move existing instances.
- **Registry**: a machine-wide file that keeps, for each instance, its path, its name, its slot and the port assigned to each variable.

The compose project name depends on the path, not on the slot. A slot that is released and then given to another worktree therefore never picks up the old one's volumes.

## Commands

All commands act on the instance of the current folder, except `ls` and `prune`, which cover the machine. Only `up` and `run` register a checkout, assign ports and write the `.env` block; the other commands change nothing.

| Command | Effect |
| --- | --- |
| `ds up` | Registers the checkout if needed, writes the `.env` block and starts the compose services. If `devslot.toml` has a `run` key, then runs that command in the foreground. Ctrl+C stops the app; the services keep running. |
| `ds run -- <cmd>` | Registers the checkout if needed, writes the block and runs any command with the instance environment, without starting the compose. E.g. `ds run -- just dev`, `ds run -- node ace migration:run`. |
| `ds doctor` | Reviews the project without starting or writing anything, and lists the errors and warnings described in Checks. Also works on an unregistered checkout. `--json` for agents. |
| `ds env` | Prints the instance environment. `--format sh` (for `eval`) or `json`. |
| `ds info` | Name, path, ports and state of the current instance. `--json` for agents. On an unregistered checkout, says so and suggests `ds up`. |
| `ds ls` | All instances on the machine. `--json` for scripts and Tunneler. |
| `ds down` | Stops the instance services, without touching the data. |
| `ds reset [service]` | Resets the instance data: deletes the volumes of its compose services. |
| `ds rm` | Brings the checkout back to its base configuration: stops the services, deletes its containers and volumes, removes the `.env` block and releases its slot. Asks for confirmation, except with `--yes`. |
| `ds prune` | Removes the instances whose folder no longer exists, and offers to delete their containers and volumes. |

**Launching the app.** `ds up` guesses nothing: it runs the `run` command of `devslot.toml`, for example `node ace serve --hmr`. Without a `run` key, `up` stops after the services; `ds run -- <cmd>` then runs any process, in Node, Python, Go or anything else.

**Why `run` only launches the app.** `ds` manages the compose services. The `run` key must launch the app alone, not a recipe that runs `docker compose up` again. In v1 this would be harmless, but this rule prepares for the sharing of v2.

`ds up` does not run the Justfile by itself, because a recipe may already manage the compose. To keep your usual command: `ds run -- just dev`, or simply `just dev` once the block is written.

## Port discovery

The tool finds the port variables on its own, by reading the compose file and the `.env`, without config.

1. **Compose file**: the one from the config, otherwise the first that exists among `dev.compose.yaml`, `dev.compose.yml`, `compose.dev.yaml`, `compose.dev.yml`, `docker-compose.dev.yaml`, `docker-compose.dev.yml`, then the names Docker takes by default (`compose.yaml`, `compose.yml`, `docker-compose.yaml`, `docker-compose.yml`). If the chosen file is not one of these last four, `ds` also puts `COMPOSE_FILE` in the environment and in the `.env` block, with the project's `compose.override.*` if it exists. A `docker compose` run without `-f` then targets the same file.
2. **Service ports, by the sentinel method**:
   1. `docker compose config --variables` lists the variables of the model and their default values.
   2. `docker compose config --format json` is run with interpolation, giving each candidate variable a unique sentinel value (a port outside the range, different for each one).
   3. Each `published` field that contains a sentinel yields a managed variable, with its service and its container port. Docker merges the `include`, `extends` and multiple files, and interprets `${VAR:-default}`. We avoid `--no-interpolate`, known to render some port syntaxes badly.
3. **Hardcoded ports** (`"5432:5432"`): a warning that suggests the `${VAR:-5432}` syntax. They are not managed in v1.
4. **App port**: `PORT`, read from `.env`, otherwise `.env.example`, plus the variables listed in `[ports] extra`. The other `*_PORT` variables of the `.env` are left alone: they may point to an external server (SMTP, remote database).
5. **Vite HMR**: `VITE_HMR_PORT` is added if `vite` is in `package.json`.
6. **What would break isolation**: `ds up` refuses to start if the merged config contains a `container_name`, or a volume or network with an explicit `name:` or `external: true`. These resources would be shared between instances despite the separate compose project. The message names the service and the key at fault. A bind mount with an absolute path gives a warning. The full list is in Checks.

Result on your Adonis template, for an instance in slot 1:

| Variable | Source | Default | Assigned |
| --- | --- | --- | --- |
| `PORT` | `.env` | 3333 | 20050 |
| `VITE_HMR_PORT` | `package.json` (vite) | 24678 | 20051 |
| `DB_PORT` | compose, service `postgres` | 5432 | 20052 |

**Known limitation**: Vite has no environment variable for its HMR port. One line is needed in `vite.config.ts`, which falls back to the default without the tool: `server: { hmr: { port: Number(process.env.VITE_HMR_PORT) || undefined } }`.

## Checks

The same checks serve `ds up` and `ds doctor`. An error blocks `ds up`; a warning is displayed without blocking. `ds up` only checks the compose it runs, whereas `ds doctor` reviews all the compose files at the root, the production one included.

| Check | Level | Why |
| --- | --- | --- |
| `container_name` on a service | error | Two instances would want the same container name. |
| Volume or network with an explicit `name:` or `external: true` | error | It would be shared between instances despite the separate compose project. |
| `.env` missing | error | `ds` has nowhere to write its block. |
| Hardcoded published port (`"5432:5432"`) | warning | It is not managed in v1 and will conflict as soon as there is a second instance. |
| Service that loads `.env` with `env_file` and receives a managed variable without pinning it in `environment:` | warning | The container would receive a host-side port (see the rule below). |
| Bind mount with an absolute path | warning | The instances would share the same files. |
| `vite` in `package.json`, but `VITE_HMR_PORT` absent from `vite.config.*` | warning | All instances would fight over HMR port 24678. |
| `.env` not ignored by git | warning | The block would end up in a commit. |
| `.env.test` that redefines a managed variable | warning | Tests would target other ports than the app. |
| Value that contains a hardcoded port (`DATABASE_URL=…:5432/…`) | warning | It does not follow the assigned port, unless `[env]` rebuilds it. |

**Rule for `env_file`**: the block values are host-side ports. A container that loads `.env` must therefore pin its internal ports in `environment:`, which takes precedence over `env_file`. This is the case for the `app` service of your template's `compose.yml`: without these two lines, a `just prod` run in a checkout that uses `ds` gives the app `PORT=20050` and `DB_PORT=20052`, and it becomes unreachable.

```yaml
  app:
    env_file:
      - .env
    environment:
      - DB_HOST=postgres
      - DB_PORT=5432
      - PORT=3333
```

`ds doctor --json` returns the same list, with the file, the service and the key at fault for each item. The `--fix` option, which would correct the files after showing a diff, will come after v1.

## Assignment and registry

An instance keeps the same ports as long as it exists. Its bookmarks and tunnels therefore do not break from one restart to the next.

1. **Lock**: `flock` on `registry.lock`. Two agents started at the same time never take the same slot.
2. **Known instance**: it gets its ports back. A new variable takes the next free port of its slot. A variable that has disappeared keeps its port, in case it comes back.
3. **Full slot**: a clear error, which states the number of ports requested and suggests increasing the slot size in the global config. No automatic second slot.
4. **New instance**: it takes, starting from 1, the smallest slot absent from the registry whose ports are all free. A port is free if a `bind` succeeds on `127.0.0.1`, `0.0.0.0`, `::1` and `::`. An `EADDRNOTAVAIL` error on the IPv6 addresses is ignored, for machines where IPv6 is disabled.
5. **Check before every `up`**: a port occupied by the instance's own containers is normal. Occupied by another program, it is an error that names the port and, if possible, the program.
6. **Atomic write**: temporary file then `rename`, before releasing the lock.

The registry is in TOML, readable and editable by hand, in `~/.local/state/devslot/registry.toml`:

```toml
version = 1

[[instance]]
path = "/home/sonny/dev/my-links.feat-auth"
name = "my-links-feat-auth-3f2a"
slot = 1
ports = { PORT = 20050, VITE_HMR_PORT = 20051, DB_PORT = 20052 }
```

The range is set in `~/.config/devslot/config.toml`. By default, 20000 to 29999 with slots of 50 ports, i.e. 200 slots. This range stays below the Linux ephemeral port range (32768 to 60999 by default).

## Environment and devslot block in .env

The tool provides the instance values in two ways: it passes them to the process it launches, and it copies them into a delimited block at the end of `.env`. With or without `ds`, everyone therefore reads the same values.

**Managed variables**: each port variable, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE` when needed (see Port discovery), the built values of `[env]`, plus `DEVSLOT_INSTANCE` and `DEVSLOT_SLOT` in the environment of launched processes. Connection credentials, `APP_KEY` and the rest of the `.env` are never touched.

**`ds` always overwrites its own variables.** If the caller's environment already defines one with another value, for example after an `eval $(ds env)` done in another checkout, `ds` replaces it and reports it. To force a value, go through `devslot.local.toml`.

**Why this is enough**: Compose takes shell variables before those of `.env`, and `COMPOSE_PROJECT_NAME` takes precedence over the file's `name:`. Adonis gives `process.env` priority over its `.env` files.

| File | Read by Compose | Read by Adonis in dev | Read by Adonis in test |
| --- | --- | --- | --- |
| `.env` | yes | yes | yes |
| `.env.local` | no | yes | no |

This is why the block goes in `.env`: `.env.local` would be read neither by Compose nor by the tests, and would create two sources of truth.

**The block** looks like this:

```
# >>> devslot: generated automatically, do not edit by hand >>>
# To go back to the default ports: ds rm
# (also deletes the containers and data of this checkout)
COMPOSE_PROJECT_NAME=my-links-feat-auth-3f2a
PORT=20050
VITE_HMR_PORT=20051
DB_PORT=20052
# <<< devslot <<<
```

**Write rules**:

- The block is written by `ds up` and `ds run`, never by the read commands. Only the content between the markers changes, by atomic write, and every write is announced with the file path.
- If `.env` does not exist, `ds` stops and asks to create it first, for example from `.env.example`.
- A warning reports a `.env` not ignored by git (`git check-ignore`), and a `.env.test` that redefines a managed variable.
- If a managed variable already exists higher up in the file (`PORT=3333`), the block at the end is enough if Compose and Adonis keep the last value of a duplicated key. Otherwise, `ds` comments out the original line (`# PORT=3333  # devslot`) and restores it on exit. To be settled in the prototype.
- A block deleted by hand is rewritten on the next `ds up` or `ds run`. The real way out is `ds rm`.
- `ds rm` removes the block and restores the commented lines. The checkout then goes back to the default ports and the compose `name:`.
- Writing can be turned off in the global config or in `[write]` of `devslot.toml`. In that case, only commands launched by `ds run` have the instance values.

**Values that contain a hardcoded port**, like `DATABASE_URL=postgres://…:5432/app`: a warning reports them. The `[env]` section of the config lets you rebuild them from the assigned ports; they then also go in the block.

## Plugin for agents

The tool's repo contains a Claude Code plugin, an integral part of v1. Thanks to the `.env` block, an agent that forgets `ds` still targets the right ports: the plugin mainly serves to give it the right habits and its ports from the start.

- **A `devslot` skill.** A skill does not trigger on the repo's files: the model chooses it from its description. The description must therefore name the use case clearly (a repo with a compose file, a devslot block in `.env` or a `devslot.toml`). Its rules:
  - start the environment with `ds up`;
  - read ports and URLs with `ds info --json`, never guess them or hardcode them;
  - reset its data with `ds reset`;
  - never run `ds rm` or delete containers or volumes by hand: other agents work on the machine.
- **A `SessionStart` hook, optional.** If the checkout is registered, it adds the output of `ds info` to the agent's context. It registers nothing and assigns nothing.

No `CLAUDE_ENV_FILE` or `PreToolUse` hook in v1. The `.env` block already covers commands run without `ds`, and `COMPOSE_PROJECT_NAME` in this block limits a `docker compose down -v` to the current instance. These hooks would also depend on known Claude Code bugs (environment lost after a `/resume`, `CwdChanged` not firing with `EnterWorktree`).

For other agents (Codex, Cursor…), the README contains the same rules, to paste into an `AGENTS.md`.

Installation: `/plugin marketplace add Sonny93/devslot`, then `/plugin install devslot@<marketplace name>`. Both names will depend on the final name.

## Configuration file

A `devslot.toml` at the repo root, optional, committed or ignored by git as you choose. It sets how the tool treats the project, and can declare the app command with the `run` key.

A `devslot.local.toml`, ignored by git, overrides it for your machine. Priority order: the command line, `devslot.local.toml`, `devslot.toml`, the global config, then the default values.

```toml
# devslot.toml: everything is optional

# App command, run by ds up in the foreground after the services.
# It launches the app alone, never the compose: ds manages the services.
run = "node ace serve --hmr"

[compose]
file = "dev.compose.yml"   # otherwise detected

[ports]
extra = ["FRONT_PORT"]     # variables to manage in addition to PORT
ignore = ["MAILPIT_PORT"]  # compose variables to leave as they are

[env]
# values built from the assigned ports, also written in the block
VITE_API_URL = "http://localhost:${PORT}"

[write]
enabled = true             # false: nothing is written to .env
```

The `run` key is the only process the file can declare: a single command, in the foreground. `ds info` shows the origin of each value: command line, file or detection.

## JSON output and Tunneler

In v1, `ds ls --json` is a stable, versioned contract. It is the only thing Tunneler will need, and the tool has no dependency on Tunneler. The `shared` key already exists, empty in v1, so that v2 does not have to change the contract.

```json
{
  "version": 1,
  "shared": [],
  "instances": [
    {
      "name": "my-links-feat-auth-3f2a",
      "path": "/home/sonny/dev/my-links.feat-auth",
      "slot": 1,
      "services_running": true,
      "ports": [
        { "var": "PORT", "port": 20050, "kind": "web" },
        { "var": "VITE_HMR_PORT", "port": 20051, "kind": "web" },
        { "var": "DB_PORT", "port": 20052, "kind": "service", "service": "postgres", "target": 5432 }
      ]
    }
  ]
}
```

The Tunneler integration will come in v2, in two forms, both optional:

- **Suggestion**: `ds tunnels --host nova` prints the command to run on your main PC, `tunneler up nova -L 20050 -L 20051 -L 20052`. In `-L`, a single port means the same port on the remote machine, so the numbers are identical on both sides.
- **Tunneler side**: it runs `ds ls --json` on the host, through its SSH session, and offers the missing tunnels. Since instances are predictable, standby tunnels on the first slots cost nothing as long as nobody connects to them.

## Tech stack

A single Rust binary, following Tunneler's conventions: edition 2024, clippy pedantic, no `unwrap` outside tests, Linux CI, binaries published in the GitHub releases.

| Need | Choice |
| --- | --- |
| CLI | `clap` (derive) |
| Reading the compose | `docker compose config --variables`, then `config --format json` with sentinel values, read with `serde_json`. No YAML parser. |
| `.env` | `dotenvy` for reading; home-made block writing, between markers, through a temporary file and `rename` |
| Registry and config | `serde` + `toml` |
| Lock | `fs4` (`flock`) |
| Launching the app | `exec` on Unix: the command replaces the tool, so Ctrl+C and the exit code behave as without it. On Windows, later: spawn, wait, relay the code. |
| Docker | Calls to the `docker compose` command, no SDK |

On the test side, unit tests for discovery, assignment and block writing, and integration tests with a fake `docker` (a script designated by `DEVSLOT_DOCKER`) so the CI does not depend on a real daemon.

## To verify in the prototype

These are no longer design questions: a trial on the Adonis template is enough to answer them.

- [ ] The sentinel method correctly finds each port variable, with its service and its container port.
- [ ] Compose and Adonis keep the last value of a duplicated key in `.env`. Otherwise, comment out the original line.
- [ ] The `vite.config.ts` line is enough to isolate HMR.
- [ ] None of your compose files has a `container_name`, or a volume or network with `name:` or `external: true`.
- [ ] Two worktrees, `ds up` in each, then a `migration:fresh` in one: the other's database stays intact.
- [ ] An `eval $(ds env)` in A, a `cd` into B, then `ds run`: the ports are B's.
- [ ] A `just dev` run without `ds` after a first `ds up`: the compose and the app target the same ports.
- [ ] A Claude Code agent in each worktree, working at the same time.
- [ ] `ds rm` really brings the checkout back to 3333, 5432 and 24678.

**Known limitation, documented but not addressed in v1**: cookies are not isolated by port. Two instances opened on `localhost` with the same session cookie name log each other out in the browser. The named URLs of v2 solve this case, since each instance will have its own hostname.

## After v1

What is planned for v2, each piece being optional and built on the v1 core.

### Shared services

A compose service started from a public image (Postgres, MySQL, Redis…) is replaced by a shared server that the tool manages itself, on a port of slot 0, with one isolated database per instance. Sharing is done by exact image, with a small adapter per family to create each instance's database and role.

**The trap to avoid.** If `DB_PORT` equals the shared server's port and a `docker compose up` is run, Compose also starts the compose's own Postgres. It tries to publish the same port, already taken, and the start fails.

**The fix: an override outside the repo.**

- `ds` generates an override file in `~/.local/state/devslot/<instance>/override.yml`. It gives the replaced service a profile that nobody activates: Compose no longer starts it.
- **Rule: `DB_PORT` and `COMPOSE_FILE` are always written together**, in the environment as in the `.env` block. Compose reads `COMPOSE_FILE` from `.env`, so the rule also holds for a `just dev` run without `ds`. After a `ds rm`, nothing remains: Compose starts its Postgres on the default port, without conflict.
- When `COMPOSE_FILE` is set, Compose no longer loads the project's `compose.override.yml` by itself: `ds` must add it to the list if it exists.
- `depends_on`: the Compose docs confirm that a dependency on a service disabled by a profile returns an error. The override will therefore also have to remove this `depends_on` from the services that depend on it, for example with `!reset`. To validate in the v2 prototype.

Still to take over from [revision 1](https://claude.ai/artifact/TxgSxb3jrF9tM1kwQRGCGn): the per-family adapters, the Redis rule (shared only if the app reads `REDIS_DB`), the sharing of `build:` services, persistence (`persist`) and the `--isolated` option.

### Other pieces

- **Database cloning**: copy the database of one instance to another. For Postgres, `CREATE DATABASE … TEMPLATE` duplicates the database on the server side.
- **Background processes**: `ds up -d`, with logs and a global stop. Either a built-in supervisor, or PM2 underneath.
- **Named URLs**: a proxy that routes `feat-auth.my-links.localhost` to the instance's web port, which also isolates cookies. An optional portless integration (Vercel Labs) is to be compared with a home-made proxy.
- **Tunneler**: `ds tunnels`, then Tunneler reading `ds ls --json`.
- **Hardcoded ports in the compose**: the same override mechanism, to remap them.
- **`ds doctor --fix`**: fix the points detected by `ds doctor`, after showing the diff and asking for confirmation.
- **`ds rm --keep-data`**, if the need appears: go back to the default ports without deleting the volumes.
- **Local prod, to study**: devslot stays a dev tool. Support for prod run locally (the app in Docker) will only come if the need is confirmed. In the meantime, the `env_file` rule from Checks is enough for a `just prod` to work in a checkout that uses `ds`.

## Decisions

No open question remains for v1.

**Revision 3 (November 9)**:

- A delimited block at the end of `.env` replaces `.env.local` and `.env.test.local`. It contains the ports, `COMPOSE_PROJECT_NAME`, `COMPOSE_FILE` when needed, and a comment that gives `ds rm`.
- `ds` always overwrites its own variables, with a warning.
- `ds rm` removes the block, deletes the checkout's containers and data and releases its slot. A block removed by hand is rewritten on the next `ds up` or `ds run`.
- Only `up` and `run` register a checkout and write; `info`, `env` and `doctor` change nothing.
- `ds doctor` in v1, read-only, with the same checks as `ds up` on all the compose files at the root. `--fix` after v1.
- `env_file` rule: a container that loads `.env` pins its internal ports in `environment:`. The block values are host-side ports.
- devslot stays a dev tool; local prod is to be studied later.
- Port discovery uses the sentinel method, no longer `--no-interpolate`.
- App ports limited to `PORT`, `VITE_HMR_PORT` and `[ports] extra`.
- `ds up` refuses to start if a `container_name`, or an explicitly named volume or network, would break isolation.
- A single slot per instance: a full slot gives a clear error, and the slot size is set in the global config.
- Free-port test also on IPv6 (`::1` and `::`).
- No `CLAUDE_ENV_FILE` or `PreToolUse` hook in v1.
- Removed: `ds nuke`, `ds release`, `ds skill`, the `dotenv` format of `ds env`, the automatic second slot, `ds init` (`docker init` already does that job).

**Revision 2, still valid**: no service sharing in v1; `run` key in `devslot.toml`; sharing in v2 with `DB_PORT` and `COMPOSE_FILE` always written together; the devslot name; slots of 50 ports; TOML config files.
