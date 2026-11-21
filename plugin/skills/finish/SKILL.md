---
name: finish
description: Close the current worktree task. Lands the work on the base branch with ds worktree land, then removes the session's worktree, which also stops the app and deletes the instance's containers and volumes.
disable-model-invocation: true
---

# Finish the worktree task

1. Invoking this skill is the user's go-ahead to land and remove: do not ask again.
2. If the session is not in a linked worktree (no EnterWorktree, or the main checkout), there is nothing to land or remove. If the checkout has a registered devslot instance whose app or services run, offer `ds down`. Stop.
3. If there are uncommitted changes, stop and tell the user: they decide whether to commit with their commit flow or discard. Do not commit on your own unless the user said so in the same message.
4. Run `ds worktree land`. When it refuses because the base moved: run `git rebase <base>` in the worktree, resolve the conflicts, rerun the project's checks (the project's CLAUDE.md says which), commit, and run `ds worktree land` again. When a conflict is not obvious to resolve, stop and ask. Never push, never force.
5. Once the land succeeded, leave with ExitWorktree, action `remove`, `discard_changes: true`. This stops the app and deletes the instance's containers and volumes: no `ds down` before.
6. If ExitWorktree says the worktree was kept, run `ds ls`, which says why, and tell the user.
7. Report in two lines: what moved on the base (the land output), and that the worktree and its environment are gone (or why not).
