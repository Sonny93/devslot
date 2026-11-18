# Make a project work with devslot

The only change devslot recommends, never requires: write the ports a compose file publishes as `${VAR:-default}`, so the default ports still work without devslot.

```yaml
services:
  postgres:
    ports:
      - '${DB_PORT:-5432}:5432'
```

`ds doctor` reports a literal port (`"5432:5432"`); ds does not remap it.

devslot never changes your project. When the app hardcodes a port or a URL, or needs several processes, the project declares it in `devslot.toml` (`run`, `[ports] extra`, `[env]`, `[web] port`) and `ds doctor` says what to change. The sections below cover the usual cases.

## Vite

`VITE_HMR_PORT` is allocated whenever Vite is detected, because a custom server may read it outside `vite.config`. Vite itself uses the HMR port 24678 only in middleware mode (`server.middlewareMode`), when another server hosts it. A standalone dev server serves HMR on its own `server.port`, so it needs nothing for HMR: see the standalone section below.

In middleware mode, Vite has no environment variable for its HMR port. Add one line to `vite.config.ts`, which falls back to the default without devslot. From Vite 8, where `server.hmr.port` is deprecated:

```ts
server: { ws: { port: Number(process.env.VITE_HMR_PORT) || undefined } }
```

Before Vite 8, use `server: { hmr: { port: Number(process.env.VITE_HMR_PORT) || undefined } }`. AdonisJS projects that use `@adonisjs/assembler` need nothing: it sets `VITE_HMR_PORT` for the dev server itself, and `ds doctor` does not warn.

### Standalone Vite server

A Vite dev server that listens on its own port (a SPA with `server.port: 3333` that proxies to the API) hardcodes two ports: its own and the proxy target. Declare the front port in `[ports] extra` and in `.env.example`, then read both ports from the environment, with the old literals as fallback so the project still works without devslot:

```toml
[ports]
extra = ["WEB_PORT"]
```

```ts
import { defineConfig, loadEnv } from 'vite'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '../..', '')
  return {
    server: {
      port: Number(env.WEB_PORT) || 3333,
      strictPort: true,
      proxy: { '/api': `http://localhost:${env.PORT || 3334}` },
    },
  }
})
```

`strictPort` makes Vite fail instead of silently taking another port when the slot port is busy. Vite reads only the `.env` of its own folder: in a monorepo, where the block is in the root `.env`, load that file with `loadEnv` and the folder of the root, for the keys you need. Declare the keys in `.env.example` too, so `ds doctor` knows them.

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

## Per-instance host and cookies

Cookies ignore the port: two instances on `localhost` share their session, XSRF and OAuth state cookies and log each other out. Suffixing cookie names cannot reach `XSRF-TOKEN` or the state cookies of libraries. A host per instance isolates all of them, with no change in the app.

Name the web port in `devslot.toml` and build the URLs the app needs on the instance host:

```toml
[ports]
extra = ["WEB_PORT"]

[web]
port = "WEB_PORT"

[env]
APP_BASE_URL = "http://${DEVSLOT_HOST}:${WEB_PORT}"
```

`ds info --json` then gives `app_url` (`http://<DEVSLOT_HOST>:<port>`), the address to open in a browser. `DEVSLOT_HOST` is `<name>.localhost`: Chrome and Firefox resolve it to loopback and treat it as secure. Tools that are not browsers need `nss-myhostname` or `systemd-resolved`, and it does not work from a remote device or through a tunnel. Details: [Configuration](configuration.md#per-instance-host-and-web-port).

## Several processes

`run` is one command. When the app needs several processes (API, queue worker, front), make `run` a single command that keeps them as foreground children and stops them together, for example `concurrently -k "node ace serve" "node ace queue:work"`, or a script that traps the signal and waits. `ds down` and a worktree removal stop the instance's processes (same user, working directory inside the checkout, not `ds` or its ancestors, a listener on a registered port or `DEVSLOT_INSTANCE=<name>` in the environment). A process that detaches (the pm2 daemon, `nohup ... &`) escapes both and keeps running: use pm2 in `pm2-runtime` mode, or leave the daemon out of dev.

## Fixed OAuth callbacks

An OAuth provider accepts only the callback URLs registered with it, so one URL cannot follow every instance, and the callback lands on that URL, not on the instance host. The state cookie was set on the instance host, so it is absent there. That holds for a separate callback variable and for forcing `PORT` in `devslot.local.toml` alike.

The flow that works:

1. Keep the callback variable on the registered URL (for example `OAUTH_CALLBACK_URL=http://localhost:3333/callback`). The code exchange keeps sending that registered `redirect_uri`.
2. Start the login from the instance's `app_url`. After the provider redirects to the registered URL, replace the origin in the address bar with the origin of `app_url`, keeping the path and the query. The state cookie is then present and the exchange succeeds.

This needs the request on the registered URL to keep its URL. When another instance (often the main checkout) listens there and redirects on a state mismatch, the code is lost. So in development only, when its state cookie is absent, the app should show the callback URL instead of redirecting, so it can be reopened on the instance host.

The files `devslot.toml` and `devslot.local.toml` are described in [Configuration](configuration.md).
