# Configuration

Reference of the configuration files of devslot. Priority: `devslot.local.toml`, `devslot.toml`, the global config, the defaults. Everything is optional.

`devslot.toml` at the repository root (committed or not), overridden on your machine by `devslot.local.toml` (ignored by git):

```toml
# App command, run by ds up (not ds up --no-run) in the foreground after the services.
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
require = []                # path globs of the main checkouts where agents must work in a linked worktree; empty means the guard is off
require_exclude = []        # wins over require, e.g. ["~/dev/scratch/**"]
```

The registry of the machine is `~/.local/state/devslot/registry.toml`. `XDG_CONFIG_HOME` moves the global config (`devslot/config.toml`, default `~/.config`) and `XDG_STATE_HOME` moves the registry (`devslot/registry.toml`, default `~/.local/state`).

## Worktree guard

`[worktrees] require` lists the path globs (same syntax as `[auto] include`, `~` allowed) of the checkouts the worktree guard covers; `require_exclude` removes some of them. Unlike `[auto]`, an empty `require` covers nothing: the guard is off until you name what it covers. An invalid glob is a configuration error that names the key (`[worktrees] require` or `[worktrees] require_exclude`). What the guard does is in [Worktrees](worktrees.md#worktree-guard).

To switch the guard off for one covered checkout, set the key in that checkout's `devslot.local.toml`:

```toml
[worktrees]
require = false
```

Only an untracked `devslot.local.toml` is read for this key. The key in `devslot.toml`, or in a `devslot.local.toml` that git tracks, is ignored and `ds doctor` warns about it. The reason: `devslot.toml` comes with the repository, often one you have not reviewed, and it would let that repository turn off a protection the user asked for. An untracked local file is yours. A `devslot.local.toml` that cannot be read or parsed cannot be used for this key: the hook then cannot decide, lets the call go and tells the agent.
