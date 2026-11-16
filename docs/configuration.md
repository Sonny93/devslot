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
```

The registry of the machine is `~/.local/state/devslot/registry.toml`. `XDG_CONFIG_HOME` moves the global config (`devslot/config.toml`, default `~/.config`) and `XDG_STATE_HOME` moves the registry (`devslot/registry.toml`, default `~/.local/state`).
