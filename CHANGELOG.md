# Changelog

All notable changes to devslot are written here for the people who use it. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/) (0.x: a minor version may break compatibility).

## [Unreleased]

### Added

- Port slots and a compose project per checkout: each checkout or worktree of a project gets its own block of ports and its own compose project, so two instances never share containers, volumes or ports. A machine-wide registry keeps every instance apart.
- A delimited block at the end of the project's `.env` carries the instance's ports, `COMPOSE_PROJECT_NAME` and `DEVSLOT_*` values, so Compose, the app and the tests read them with or without `ds`. devslot only writes in a `.env` that git ignores.
- `ds up`, `ds down`, `ds run`, `ds env`, `ds ensure` and `ds reset` start, stop and reset the instance and run commands with its environment.
- `ds info`, `ds ls` and `ds doctor` show the instance (ports, URLs, services), list every instance of the machine, and review a project without starting or writing anything. `ds rm` and `ds prune` delete the data of an instance and opt a checkout out.
- `devslot.toml` and `devslot.local.toml` declare what a project needs: the command to run, extra ports, environment variables and a per-instance host (`<name>.localhost`) that keeps cookies apart.
- Worktrees: `ds worktree land` fast-forwards the main checkout's branch onto the worktree's branch, `ds worktree rm` removes a worktree and its instance, and an optional worktree guard keeps agents from editing the main checkout.
- A Claude Code plugin gives each session its own worktree and instance through hooks, brings a `devslot` skill with the rules for agents, and adds the `/devslot:finish` command that lands the work and removes the worktree.
- `ds ls --json` and `ds info --json` give scripts and other tools a stable, versioned description of the instances.
- Tunneler integration: with a `[tunneler] host` in the global config, `ds info` and the session start give the command and the link that open the instance from your computer.
- Packages for Debian and Ubuntu (`.deb`), Arch Linux (`.pkg.tar.zst`) and other Linux x86_64 distributions (archive with an install script), with checksums.
