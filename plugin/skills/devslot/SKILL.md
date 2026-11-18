---
name: devslot
description: Use in a repository that has a compose file, a devslot block in .env or a devslot.toml, and whenever you need this checkout's own ports, URLs, database or services (start the stack, find the database port or the app URL, reset the data, run a command with the instance environment). Several checkouts and agents share this machine; devslot gives each one its own ports and compose project.
---

# devslot

devslot gives each checkout or worktree of a project its own slot of ports and its own compose project, and writes them in a block at the end of `.env`. Other agents work on the same machine at the same time, so everything you start or delete must stay inside this checkout's instance.

## Rules

- Start the services with `ds up --no-run`. Plain `ds up` then runs the `run` key of `devslot.toml` in the foreground and does not return: use it only as a background command, when you also want the app. To run a command with the instance environment, use `ds run -- <command>`.
- Read ports and URLs with `ds info --json`. Never guess or hardcode them: 3333, 5432 and 24678 are the defaults of other instances.
- Reset the data with `ds reset` (everything) or `ds reset <service>` (one service). It deletes the data and starts the services again.
- Never run `ds rm`, `ds prune`, `ds worktree rm`, `docker compose down -v`, `docker volume rm` or remove containers by hand: that can delete another agent's data. Ask the user when something has to be removed.
- The guard refuses any command whose text contains `ds rm`, `ds prune`, `ds worktree rm` or `ds hook`, even inside a quoted string. To search for one of them in files, use a pattern such as `ds.rm` instead of `ds rm`.
- When the devslot worktree guard is on (the session start says so), the main checkout is for reading and searching only: before editing files or running `ds up`, `ds run` or `ds reset`, enter a worktree with the EnterWorktree tool, with a short name taken from the task, and never work around a refusal through Bash. Run `ds worktree land` only after the user's explicit go-ahead on the committed work: it is fast-forward only, so when the base moved, run `git rebase <base>` in the worktree, resolve the conflicts, commit, and run `ds worktree land` again. Never push.
- When the user asks to remove this session's worktree, leave it with the ExitWorktree tool, action `remove` and `discard_changes: true` (Claude Code cannot check a worktree devslot created; devslot's own hook keeps a worktree that holds unsaved work or that another session uses). Never leave with `keep` and then hand the user a removal command. When ExitWorktree says the worktree was kept, run `ds ls`, which says why, and tell the user.
- If `ds info` says the checkout is opted out, do not run `ds up` without asking the user first: `ds rm` opted it out on purpose.
- `DEVSLOT_INSTANCE` names this instance. Show it in logs or on a health endpoint when that helps tell instances apart.
- When the checkout cannot be isolated (a compose file with `container_name`, a literal port), `ds doctor` explains why and what to change.

## Good to know

- `ds info --json` gives the `url` of each web port: use it instead of building `http://localhost:<port>` yourself.
- `ds down` stops the services and keeps the data.
- The block at the end of `.env` already gives the instance's ports to commands that read `.env` (the app, its tests, Compose), so plain commands work without `ds run`. Never edit the block by hand: ds rewrites it.
