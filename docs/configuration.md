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
# values built from the ports and the host of the instance, also written in the block
VITE_API_URL = "http://localhost:${PORT}"
APP_BASE_URL = "http://${DEVSLOT_HOST}:${WEB_PORT}"

[web]
port = "WEB_PORT"          # the managed port variable that is the app opened in a browser

[write]
enabled = true             # false: nothing is written to .env
```

## Per-instance host and `[web] port`

Each instance has a host, `DEVSLOT_HOST`, of the form `<label>.localhost`. The label comes from the instance name (`_` becomes `-`, at most 63 characters keeping the hash, `-<slot>` appended when another instance already uses it). It is stored in the registry (`host`, and `web_port` for the `[web] port` variable) when the instance is registered and never recomputed, so a rename or a new instance never changes it. When another instance already has the label, `-<slot>` is appended, then `-<slot>-<n>`. An entry written by an older version gets its host at the next registration refresh. `DEVSLOT_HOST` is in the `.env` block, in `ds env`, in the environment of `ds run`, `ds up` and the `run` key, and `${DEVSLOT_HOST}` works in `[env]` values like a port variable. `ds info --json` and `ds ls --json` give it as `host`.

`[web] port` (in `devslot.toml` or `devslot.local.toml`) names one managed port variable. The `url` of that port becomes `http://<DEVSLOT_HOST>:<port>` and `ds info --json` gets `app_url` with the same value. The other ports stay on `localhost`. The value is only ever a variable name, never a host or a URL: agents open `url` in a browser that may be logged into your accounts, so a committed file must not choose where they go. A name that is not a managed web port (unknown, or a port of kind service such as a database) gives a `ds doctor` warning (`web_port_unmanaged`) and every url stays on `localhost`.

## Forcing a port

Only `devslot.local.toml` may set a managed port variable in `[env]` (`WEB_PORT = "3333"`): the value is used as written, the app listens on it, and it is written to the `.env` block. The registry keeps the port it allocated, so that port stays reserved. What ds prints and checks is the effective port: the forced value when it is canonical (a number from 1 to 65535 without sign or leading zero, such as `3333`; `0`, `03333` or `abc` are not), else the allocated port. `ds info`, `ds ls`, the session context and the busy-port check of `ds up` use the effective port, and `url` and `app_url` follow it. `ds info --json` and `ds ls --json` give a forced port `"forced": true` and the text output says `(forced in devslot.local.toml)`. A forced port lies outside the slot `range` and may be shared with another instance, so do not key on the port alone. `ds ls` reads only the `devslot.local.toml` of each instance, and shows the allocated ports of one whose file or folder is missing, unreadable or malformed. A forced port from a `devslot.local.toml` that git tracks is ignored (that file came with the repository, not from you), and so is every forced port when git cannot tell; git is asked only when the file holds a canonical forced port. `ds ensure` refuses to write the block while the file is tracked. `ds down` and the removal of a worktree still act on the registry ports only.

Browsers (Chrome, Firefox) resolve `*.localhost` to the loopback address and treat it as a secure context. Tools that are not browsers (`curl`, an HTTP client in a test) need `nss-myhostname` or `systemd-resolved` to resolve it. Through an SSH tunnel that listens on the same port on your computer (`tunneler up <host> -L <port>`, or `ssh -L <port>:127.0.0.1:<port> <host>`; `tunneler devslot <host> <instance>` does it in one step, see [Open an instance from your computer](#open-an-instance-from-your-computer-tunneler)), the browser of that computer opens `app_url` as is, and each instance keeps its own cookies; a tunnel to another local port works, but the link of `app_url` then no longer matches. It does not work from a device that has no tunnel, such as a phone on the network. How to use it with cookies: [Make a project work with devslot](project-setup.md#per-instance-host-and-cookies).

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

[tunneler]
host = "vega"       # the SSH alias of this server on your computer; absent means off
```

The registry of the machine is `~/.local/state/devslot/registry.toml`. `XDG_CONFIG_HOME` moves the global config (`devslot/config.toml`, default `~/.config`) and `XDG_STATE_HOME` moves the registry (`devslot/registry.toml`, default `~/.local/state`).

## Open an instance from your computer (Tunneler)

`[tunneler] host` in the global config is the SSH alias of this server, as your computer knows it. It is opt-in: without it, nothing changes and no output or JSON key mentions Tunneler. With it, devslot prints, for each registered instance, a command and a link that open the instance from your computer through Tunneler:

```
tunneler devslot vega web-shop-098a
tunneler://devslot/vega/web-shop-098a
```

The host and the instance name (the `name` of `ds ls --json`) are percent-encoded in the link. Both must hold 1 to 64 characters for the host and 1 to 255 for the name, only `A-Za-z0-9._-`, starting with a letter or a digit (the same bounds as Tunneler). devslot prints no hint for an instance whose name is outside them.

Where it shows up: the section "Open from your computer (Tunneler)" of `ds info`, the `tunneler` key (`command` and `url`) of `ds info --json`, and the SessionStart context, so the agent gives you the command and the link next to the URL of the instance. `ds ls --json` is unchanged. devslot only prints text and never calls Tunneler.

An invalid value never breaks anything else: `ds info` prints an error on stderr that names `[tunneler] host`, the file and the reason, then the report without the hint and exits 0; the SessionStart context tells the agent the value is invalid; `ds doctor` reports an error (`tunneler_host_invalid`). The worktree guard, `ds up` and the other commands go on as before. A global config that cannot be read at all gives a warning on stderr in `ds info` and no hint.

One alias serves every computer, so each of them must name the server the same way in its SSH config. The link needs a version of Tunneler that has the `devslot` command.

## Worktree guard

`[worktrees] require` lists the path globs (same syntax as `[auto] include`, `~` allowed) of the checkouts the worktree guard covers; `require_exclude` removes some of them. Unlike `[auto]`, an empty `require` covers nothing: the guard is off until you name what it covers. An invalid glob is a configuration error that names the key (`[worktrees] require` or `[worktrees] require_exclude`). What the guard does is in [Worktrees](worktrees.md#worktree-guard).

To switch the guard off for one covered checkout, set the key in that checkout's `devslot.local.toml`:

```toml
[worktrees]
require = false
```

Only an untracked `devslot.local.toml` is read for this key. The key in `devslot.toml`, or in a `devslot.local.toml` that git tracks, is ignored and `ds doctor` warns about it. The reason: `devslot.toml` comes with the repository, often one you have not reviewed, and it would let that repository turn off a protection the user asked for. An untracked local file is yours. A `devslot.local.toml` that cannot be read or parsed cannot be used for this key: the hook then cannot decide, lets the call go and tells the agent.
