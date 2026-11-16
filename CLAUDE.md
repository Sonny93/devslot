# devslot

Rust CLI `ds`: per-checkout port slots and compose projects, plus a Claude Code plugin (`plugin/`) whose hooks give each session its own worktree.

## Verify

- `cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked` (what CI runs). The toolchain is pinned in `rust-toolchain.toml`; a `RUSTUP_TOOLCHAIN` in the environment overrides the pin, so unset it before checking.
- Tests never call the real Docker: they point `DEVSLOT_DOCKER` at `tests/support/fake-docker.sh` and isolate `XDG_CONFIG_HOME`/`XDG_STATE_HOME`.
- The hooks run the installed `ds`, not `target/`: run `cargo install --path . --locked` before trying a hook change in a real session.

## Rules

- The hard constraints and stakes are in the README "Why" section (no `.env` value ever printed, reading a repo never runs its code, never touch containers or volumes devslot did not create for this checkout). Every change keeps them.
- The agent rules exist in four copies that change together: `plugin/skills/devslot/SKILL.md`, the README "Rules for agents" block, `GUIDANCE` in `src/commands/hook/session_start.rs` and `DENY_REASON` in `src/commands/hook/pre_tool_use.rs`. A unit test in `src/commands/hook/mod.rs` checks they name the same commands.
- Every decision gets a dated row in `docs/decisions.md`. A superseded row is not edited: the new row says what it replaces. Check it before proposing an option, rejected ones are recorded there.

## Gotchas

- The installed PreToolUse hook denies any Bash command whose text contains `ds rm`, `ds prune`, `ds worktree rm` or `ds hook`, quoted or not: search with a pattern such as `ds.rm`, and run those commands through the test suite rather than by hand.
