# Make a project work with devslot

The only change devslot recommends, never requires: write the ports a compose file publishes as `${VAR:-default}`, so the default ports still work without devslot.

```yaml
services:
  postgres:
    ports:
      - '${DB_PORT:-5432}:5432'
```

`ds doctor` reports a literal port (`"5432:5432"`); ds does not remap it.

Vite has no environment variable for its HMR port. Add one line to `vite.config.ts`, which falls back to the default without devslot. From Vite 8, where `server.hmr.port` is deprecated:

```ts
server: { ws: { port: Number(process.env.VITE_HMR_PORT) || undefined } }
```

Before Vite 8, use `server: { hmr: { port: Number(process.env.VITE_HMR_PORT) || undefined } }`. AdonisJS projects that use `@adonisjs/assembler` need nothing: it sets `VITE_HMR_PORT` for the dev server itself, and `ds doctor` does not warn.

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

The files `devslot.toml` and `devslot.local.toml` are described in [Configuration](configuration.md).
