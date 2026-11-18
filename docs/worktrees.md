# Worktrees

How devslot creates, uses and removes the worktrees of Claude Code sessions. It needs the plugin (see [Install](../README.md#install)).

## Worktree creation

The `WorktreeCreate` hook replaces Claude Code's own worktree creation: `ds hook worktree-create` runs `git worktree add` itself and puts the worktree in `~/dev/.worktrees/<repo>/<name>`, on a branch called `<name>` (the existing one when there is one, else a new one from the branch checked out in the main worktree). For a new branch, the hook records that base in `git config branch.<name>.devslotBase` (a warning on stderr tells you to pass `--onto` to `ds worktree land` when it cannot); for a reused branch it announces `ds: reusing the existing branch <name>: <n> commit(s) not in <base>`. Change the folder with `[worktrees] root` in the global config (see [Configuration](configuration.md)). Without the plugin, Claude Code creates its worktrees where it always did.

Creating a worktree with a name that already exists reopens it: `ds hook worktree-create` prints its path and registers it if needed, like `claude --worktree <name>` does natively. A locked worktree is not reopened: one left half-created by an interrupted creation (git's `initializing` lock) is refused with the commands to remove it, any other lock asks for `git worktree unlock` first.

The hook stops its own work before Claude Code kills it (180 s). `git worktree add`, including the repository's `post-checkout` hook, gets what is left of the budget minus 20 s kept for registration. When the add runs out of time during the checkout itself, the half-created worktree is removed, and the branch too when this creation made it. When it runs out during `post-checkout`, the worktree is complete: it is kept, ds warns that the hook did not finish, and the path is printed. When registration has no time left, the first agent command registers the worktree.

### Sessions started with `claude --worktree <name> --tmux`

Claude Code creates the worktree before plugins load, so the plugin's `WorktreeCreate` hook does not run. If you start sessions that way, declare the `WorktreeCreate` and `WorktreeRemove` hooks yourself in `~/.claude/settings.json` (same commands as `plugin/hooks/hooks.json`):


```json
{
  "hooks": {
    "WorktreeCreate": [
      { "hooks": [{ "type": "command", "command": "ds hook worktree-create", "timeout": 180 }] }
    ],
    "WorktreeRemove": [
      { "hooks": [{ "type": "command", "command": "ds hook worktree-remove", "timeout": 180 }] }
    ]
  }
}
```

Claude Code does not merge a plugin's hook with the same hook declared in settings: for every session started without `--tmux`, both copies run, in parallel. ds takes a lock per worktree, so this is supported: the second creation waits for the first and prints the same path, and the second removal leaves the work to the first one.

Headless sessions (`claude -p`) never fire `WorktreeRemove`: their worktrees stay until you remove them by hand.

## Worktree lifecycle

With the plugin, a Claude Code session in a worktree goes like this:

1. `WorktreeCreate`: `ds hook worktree-create` creates the worktree, then registers it (slot, `.env`, block). It never starts services.
2. `PreToolUse`: before each Bash command and, with the worktree guard on, before each file write (see [Worktree guard](#worktree-guard)), `ds hook pre-tool-use` registers a checkout that is not (worktrees entered mid-session included), tells the agent about its instance, and denies `ds rm`, `ds prune`, `ds worktree rm` and every `ds hook` command (Claude Code runs the hooks itself, never through the Bash tool, so this only stops an agent calling one by hand). The guard reads the text of the command, not what the shell would run, so it also refuses a command that only quotes these words (for example `grep -n "ds rm" README.md`, or a commit message that mentions `ds hook`); it is a guard rail, not a sandbox.
3. `SessionStart`: `ds hook session-start` adds the instance to the agent's context.
4. `WorktreeRemove`: `ds hook worktree-remove` removes the worktree the way `ds worktree rm` does (see [Commands](commands.md)): it refuses a locked worktree and a worktree that another session or program still uses (see [Worktrees in use](#worktrees-in-use-linux-only)), stops the instance's processes, removes the git worktree, deletes the instance's containers, volumes and networks by label, forgets the instance, and deletes the branch when it is named like the worktree folder and `git merge-base --is-ancestor` confirms it is merged into the `HEAD` of the main checkout. It first refuses a worktree that holds unsaved work (see [Unsaved work](#unsaved-work-blocks-the-removal-hook)). When Docker fails, the registry keeps the entry and `ds prune` cleans it later.

## Worktree guard

The guard keeps agents from working in the main checkout of a repository: there they read and search, and the work happens in a linked worktree. It is off until `[worktrees] require` in the global config lists the checkouts it covers (see [Configuration](configuration.md#worktree-guard)). `require_exclude` removes some of them, and an untracked `devslot.local.toml` with `[worktrees] require = false` switches it off for one checkout.

The workflow, end to end:

1. The session starts in the main checkout. `ds hook session-start` adds a line to the context (see below) so the agent reads there and enters a worktree before writing.
2. The agent calls the `EnterWorktree` tool with a short name taken from the task. The `WorktreeCreate` hook creates the worktree and records its base branch.
3. The agent writes code and commits in the worktree, where writes and `ds up`, `ds run` and `ds reset` are allowed.
4. When the user approves the committed work, the agent runs `ds worktree land` (see [Land the work](#land-the-work-on-the-main-checkout)). It never pushes: that stays with the user.
5. When the session ends, the `WorktreeRemove` hook removes the worktree, unless it holds unsaved work.

What is refused, by `ds hook pre-tool-use`:

- A call of `Edit`, `Write`, `MultiEdit` or `NotebookEdit` whose target file (`file_path`, or `notebook_path`) lies in a covered main checkout. A target in a linked worktree, or outside any git repository, goes. The target path is resolved first: a relative path is read from the `cwd` of the call, a file that does not exist yet resolves like one that does, and symbolic links are followed (up to 40, then the guard cannot decide). The refusal tells the agent to enter a worktree with `EnterWorktree`; when the session already works in a linked worktree of the same repository, it names the equivalent path there; for a subagent, which must not call `EnterWorktree`, it asks to report back to the main session.
- A Bash command that runs `ds up`, `ds run` or `ds reset` (found the way the `ds rm` refusal finds its commands, so `ds run -- ds reset` counts), when the `cwd` of the call is in a covered main checkout. Only the `cwd` is judged, not a `cd` inside the command. Other Bash commands are not analysed: `sed -i` or `git commit` in the main checkout go. The guard is a guard rail, not a sandbox.

The guard refuses only on a positive finding. When it cannot decide (git fails or times out, the global config or `devslot.local.toml` cannot be read, too many symbolic links), the call goes and the agent gets `the devslot worktree guard could not decide (<reason>); tell the user` as context. The same holds when the hook cannot read its input (`the devslot hook could not read its input (<reason>); tell the user`); a `Write` carries the whole file, so the hook accepts up to 32 MiB on stdin. With an empty `require` the hook touches no file and runs no git.

The write tools have their own `PreToolUse` entry in `plugin/hooks/hooks.json` (matcher `Edit|Write|MultiEdit|NotebookEdit`, timeout 10 s) because they run on every edit: they get a 5 s budget, never call `ds ensure` and never touch Docker, where the Bash entry keeps its 15 s budget. A run that runs out of time says nothing.

A covered main checkout is not registered: `ds ensure`, and so the hook, leaves it alone, because nobody runs an instance there. A main checkout that was registered before the guard covered it is left as it is, and `ds doctor` says so. `ds doctor` also reports whether the guard is active for the checkout and warns when `devslot.toml` sets `[worktrees] require`, which is ignored.

`ds hook session-start` adds one line when the guard is on: in a covered main checkout, to read there, enter a worktree before editing or starting an environment, and run `ds worktree land` when the user approves (with the number of kept worktrees of the repository when there are some, listed by `ds ls`); in a linked worktree, to work there; and, when `devslot.local.toml` switches it off, that the guard is off for the checkout.

## Land the work on the main checkout

`ds worktree land` brings the committed work back to the branch checked out in the main checkout, without a push. It only fast-forwards: the worktree's branch must contain the target branch. The target is the base recorded when the worktree was created, or `--onto <branch>`. The agent runs it only after the user's explicit go-ahead on the committed work.

When the base moved since the worktree was created, `ds worktree land` refuses and says how to continue:

```sh
git rebase <base>      # in the worktree, resolve the conflicts, commit
ds worktree land
```

It runs `git merge --ff-only` in the main checkout, so a landing updates that checkout's files and runs the repository's `post-merge` hook; an uncommitted change of yours there that the landing touches makes git refuse, and nothing is landed. It also refuses when the branch adds a file that exists in the main checkout without being tracked there (untracked or ignored, such as a local `.env`), instead of overwriting it. The full list of refusals and the exit codes are in [Commands](commands.md).

Landing does not remove the worktree: the branch and the folder stay until the session ends or you run `ds worktree rm`.

## Unsaved work blocks the removal hook

The `WorktreeRemove` hook never deletes uncommitted changes or commits found nowhere else (reachable from the worktree's `HEAD` only, as in the `ds worktree rm` preview), whatever was answered to Claude Code. It counts them before stopping any process, so a refusal stops and deletes nothing, and again right before `git worktree remove`, because work can appear while processes stop. The counts run under a 150 s budget (Claude Code kills the hook at 180 s); a count that cannot finish refuses the removal too. The worktree stays, `ds ls` lists it with the reason `it holds unsaved work`, and the message points to `ds worktree rm <path>` from a terminal, which previews the work and needs `--force` to delete it. Claude Code does not show the hook's message (see [Known limitations](../README.md#known-limitations)). Commit the work, `ds worktree land` it, or remove the worktree by hand.

When the user asks an agent to remove its own worktree, the agent calls `ExitWorktree` with action `remove` and `discard_changes: true`: Claude Code cannot verify the state of a worktree that devslot created, so it refuses `remove` without `discard_changes`, even for a clean worktree. With it, Claude Code deletes nothing itself and hands the removal to the `WorktreeRemove` hook, which decides what is kept (unsaved work, a worktree another session uses). `ExitWorktree` then only says the worktree was kept, without the reason: `ds ls` gives it.

## Worktrees in use (Linux only)

Another Claude Code session can work in the same worktree (`claude -w <name>` reopens it), and closing one session must not delete what the other still uses. Before anything is stopped or deleted, and again right before `git worktree remove` (stopping can take seconds, and `ds worktree rm` may wait at a prompt), removal looks for processes of somebody else running in the worktree: same user, working directory in the worktree, not `ds` or one of its ancestors. When it finds any, nothing is deleted (the worktree, its data and its slot stay), the message lists them as `pid (name)` and the exit code is 1; Claude Code then keeps the worktree. For the hook, the closing session's own processes are not counted: its `claude` process (taken from `CLAUDE_PID` when that is an ancestor of `ds`, else the nearest ancestor named `claude`; without either, nothing counts as the session's own) and everything below it, such as its MCP servers and subagents. Servers of the instance started by another session do count. Orphaned daemons (an `adb` server, an ssh master: parent process 1 or `systemd`) never count, but a `claude` process always does. For `ds worktree rm`, the instance's own processes are shown in the preview and stopped after confirmation, so only the other processes count. `ds worktree rm --force <name>` skips the check; the hook has no way to skip it.

Processes alone miss a session that entered the worktree with a `cd`: its `claude` stays in the main checkout (Claude Code puts the Bash working directory back after each command that leaves the project), and once idle it may have only containers running. So `ds up`, `ds run` and `ds reset`, run in a linked worktree under a Claude Code session (found the same way as above, and named `claude`: `ds doctor` warns when it is not), also leave a lease: a file in the state folder named after that session's `claude` pid and start time, holding its session id. The lease lives as long as that `claude` process, so it covers `/clear` and the session's subagents, and it never expires (an expiry would delete the data of an idle session, the case it protects). At both checks, a live lease (the pid runs, is named `claude` and started at the same time) of another session refuses the removal like a foreign process, and the message names the session; dead leases are deleted. The closing session's own lease does not count for the hook; for `ds worktree rm` every live lease counts, including from a `! ds worktree rm` typed inside a session, and the `ds worktree rm` preview lists the live leases on its `In use` line (`session <id>`, else `claude <pid>`) and refuses before asking for the confirmation, like a foreign process; the check is repeated after the confirmation, because a lease can appear in between. `--force` skips this check too. A removal deletes the worktree's leases, creating a worktree clears any left at its path, and `ds prune` deletes the dead ones. Still invisible: containers started without ds (`docker compose up` by hand) and agents other than Claude Code. A session whose very first `ds up` in the worktree lands while another session is removing it, after the last check, loses the stack it was starting.

## Which processes are stopped (Linux only)

Removing a worktree first stops the processes of its instance, and only when an instance is registered for it. A process is stopped only when all of these hold:

- it runs as your user, and is not `ds` or one of its ancestors;
- its working directory is the worktree or a folder below it (compared by path component: `feat` never matches `feat-2`);
- it listens on TCP on one of the ports registered for the instance, or its environment holds exactly `DEVSLOT_INSTANCE=<instance name>` (the block puts that variable in `.env`, and `ds run` passes it). Only that marker is looked for; the environment is never printed or stored.

Each process is signalled on its own, never as a group (an editor can be the leader of the group of the dev server it started). A stopped process's descendants are stopped too, when they also run as your user with their working directory in the worktree. Signals go through a pidfd after the checks are repeated, so a reused pid is never hit. They get SIGTERM and up to 5 seconds to exit; ds then looks again (children that were respawned and carry the marker or listen on a port) for at most 3 rounds, and sends SIGKILL to what is left. A parent that is not a target and respawns its child (for example `node ace serve` started by hand, without `ds run`) is listed as still running, not stopped: servers started through `ds run` or `ds up` carry the marker and are stopped entirely, the others may need stopping by hand. ds prints `ds: stopped <pid> (<name>)` for each, with the kernel name of the process, never its command line. Every other process whose working directory is in the worktree (a tmux server, an ssh master connection, an editor, another session) is listed on a `ds: still running in <path>: ...` line and never signalled (when the removal goes on, they are orphaned daemons or the removal was forced). On other systems nothing is stopped and nothing is listed.

If `git worktree remove` fails after git has already forgotten the worktree (typically "Directory not empty" because a process recreated files), the removal goes on and ds warns that the folder is left on disk; it never deletes it itself.
