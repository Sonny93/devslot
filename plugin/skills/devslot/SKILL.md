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
- If `ds info` says the checkout is opted out, do not run `ds up` without asking the user first: `ds rm` opted it out on purpose.
- `DEVSLOT_INSTANCE` names this instance. Show it in logs or on a health endpoint when that helps tell instances apart.
- When the checkout cannot be isolated (a compose file with `container_name`, a literal port), `ds doctor` explains why and what to change.
