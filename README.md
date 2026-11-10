# devslot

Run several instances of the same dev stack on one machine: each checkout or worktree gets its own block of ports and its own compose project.

**Status:** in development, nothing is released. Only `ds doctor` exists so far.

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
