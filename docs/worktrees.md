# Worktrees

How devslot creates, uses and removes the worktrees of Claude Code sessions. It needs the plugin (see [Install](../README.md#install)).

## Worktree creation

The `WorktreeCreate` hook replaces Claude Code's own worktree creation: `ds hook worktree-create` runs `git worktree add` itself and puts the worktree in `~/dev/.worktrees/<repo>/<name>`, on a branch called `<name>` (the existing one when there is one, else a new one from the branch checked out in the main worktree). Change the folder with `[worktrees] root` in the global config (see [Configuration](configuration.md)). Without the plugin, Claude Code creates its worktrees where it always did.

Creating a worktree with a name that already exists reopens it: `ds hook worktree-create` prints its path and registers it if needed, like `claude --worktree <name>` does natively.

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

Headless sessions (`claude -p`) never fire `WorktreeRemove`: their worktrees stay until you remove them by hand.

## Worktree lifecycle

With the plugin, a Claude Code session in a worktree goes like this:

1. `WorktreeCreate`: `ds hook worktree-create` creates the worktree, then registers it (slot, `.env`, block). It never starts services.
2. `PreToolUse`: before each Bash command, `ds hook pre-tool-use` registers a checkout that is not (worktrees entered mid-session included), tells the agent about its instance, and denies `ds rm`, `ds prune`, `ds worktree rm` and every `ds hook` command (Claude Code runs the hooks itself, never through the Bash tool, so this only stops an agent calling one by hand). The guard reads the text of the command, not what the shell would run, so it also refuses a command that only quotes these words (for example `grep -n "ds rm" README.md`, or a commit message that mentions `ds hook`); it is a guard rail, not a sandbox.
3. `SessionStart`: `ds hook session-start` adds the instance to the agent's context.
4. `WorktreeRemove`: `ds hook worktree-remove` removes the worktree the way `ds worktree rm` does (see [Commands](commands.md)): it refuses a locked worktree and a worktree that another session or program still uses (see [Worktrees in use](#worktrees-in-use-linux-only)), stops the instance's processes, removes the git worktree, deletes the instance's containers, volumes and networks by label, forgets the instance, and deletes the branch when it is named like the worktree folder and `git merge-base --is-ancestor` confirms it is merged into the `HEAD` of the main checkout. When Docker fails, the registry keeps the entry and `ds prune` cleans it later.

## Worktrees in use (Linux only)

Another Claude Code session can work in the same worktree (`claude -w <name>` reopens it), and closing one session must not delete what the other still uses. Before anything is stopped or deleted, and again right before `git worktree remove` (stopping can take seconds, and `ds worktree rm` may wait at a prompt), removal looks for processes of somebody else running in the worktree: same user, working directory in the worktree, not `ds` or one of its ancestors. When it finds any, nothing is deleted (the worktree, its data and its slot stay), the message lists them as `pid (name)` and the exit code is 1; Claude Code then keeps the worktree. For the hook, the closing session's own processes are not counted: its `claude` process (taken from `CLAUDE_PID` when that is an ancestor of `ds`, else the nearest ancestor named `claude`; without either, nothing counts as the session's own) and everything below it, such as its MCP servers and subagents. Servers of the instance started by another session do count. Orphaned daemons (an `adb` server, an ssh master: parent process 1 or `systemd`) never count, but a `claude` process always does. For `ds worktree rm`, the instance's own processes are shown in the preview and stopped after confirmation, so only the other processes count. `ds worktree rm --force <name>` skips the check; the hook has no way to skip it. A session is seen only through processes whose working directory is in the worktree: sessions started with `claude --worktree` are covered (their `claude` runs there for the whole session), but one that entered the worktree with a `cd` (its `claude` stays in the main checkout) and sits idle with only containers running is invisible.

## Which processes are stopped (Linux only)

Removing a worktree first stops the processes of its instance, and only when an instance is registered for it. A process is stopped only when all of these hold:

- it runs as your user, and is not `ds` or one of its ancestors;
- its working directory is the worktree or a folder below it (compared by path component: `feat` never matches `feat-2`);
- it listens on TCP on one of the ports registered for the instance, or its environment holds exactly `DEVSLOT_INSTANCE=<instance name>` (the block puts that variable in `.env`, and `ds run` passes it). Only that marker is looked for; the environment is never printed or stored.

Each process is signalled on its own, never as a group (an editor can be the leader of the group of the dev server it started). A stopped process's descendants are stopped too, when they also run as your user with their working directory in the worktree. Signals go through a pidfd after the checks are repeated, so a reused pid is never hit. They get SIGTERM and up to 5 seconds to exit; ds then looks again (children that were respawned and carry the marker or listen on a port) for at most 3 rounds, and sends SIGKILL to what is left. A parent that is not a target and respawns its child (for example `node ace serve` started by hand, without `ds run`) is listed as still running, not stopped: servers started through `ds run` or `ds up` carry the marker and are stopped entirely, the others may need stopping by hand. ds prints `ds: stopped <pid> (<name>)` for each, with the kernel name of the process, never its command line. Every other process whose working directory is in the worktree (a tmux server, an ssh master connection, an editor, another session) is listed on a `ds: still running in <path>: ...` line and never signalled (when the removal goes on, they are orphaned daemons or the removal was forced). On other systems nothing is stopped and nothing is listed.

If `git worktree remove` fails after git has already forgotten the worktree (typically "Directory not empty" because a process recreated files), the removal goes on and ds warns that the folder is left on disk; it never deletes it itself.
