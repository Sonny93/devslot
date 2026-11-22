# Security policy

## Supported versions

Only the latest 0.x release receives security fixes.

## Report a vulnerability

Use GitHub's private vulnerability reporting: open the Security tab of the repository and choose "Report a vulnerability". Please do not open a public issue for a vulnerability.

When it helps, paste the output of `ds doctor --json` or `ds info --json`. Never paste the contents of a `.env` file.

## Security boundary

devslot runs on a developer machine, on repositories that may not have been reviewed, and its output is read by coding agents. A report is in scope when it breaks one of these constraints:

- No `.env` value is ever printed.
- Reading a repository never runs code from it.
- devslot never stops or removes containers, volumes or compose projects it did not create for the current checkout.

The "Why" section of the [README](README.md) lists the other hard constraints.
