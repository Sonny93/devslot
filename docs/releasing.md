# Releasing

How a maintainer cuts a release of devslot. A release is cut by a GitHub Actions workflow dispatched from `main`: nothing is tagged or built by hand.

## Before the release

Every user-visible change adds a line under `## [Unreleased]` in [CHANGELOG.md](../CHANGELOG.md), in the same commit. Do not add a version heading: the release tooling turns `[Unreleased]` into the version section.

The version stays `0.0.0` in `package.json`, `Cargo.toml` and `Cargo.lock` until the first release; releases bump it, never edit it by hand.

## Cut a release

```sh
gh workflow run release.yml -R Sonny93/devslot -f increment=<patch|minor|major>
```

While devslot is in 0.x, use `minor` for a breaking change and `patch` for the rest. The first release is `minor`, which gives 0.1.0.

## What the workflow does

In order:

1. CI runs (format, clippy, tests).
2. The reusable release workflow of minimalstuff/tooling runs release-it with keep-a-changelog: it turns `## [Unreleased]` into the version section, bumps `package.json`, `Cargo.toml` and `Cargo.lock`, commits `chore(release): vX.Y.Z`, tags, pushes with the deploy key, and creates the GitHub release from the CHANGELOG section.
3. A static musl `ds` is built from the tag, then packaged and attached to the release: `devslot_<v>_amd64.deb`, `devslot-<v>-1-x86_64.pkg.tar.zst`, `devslot-<v>-x86_64-linux.tar.gz` and `SHA256SUMS`.

The release exists a few minutes before its assets do. A musl smoke build before the tag guards the build step.

## One-time GitHub setup

- Environment `release`, limited to `main`, holding the secret `RELEASE_DEPLOY_KEY`.
- Deploy key "release" with write access to the repository.
- Tag ruleset "Release tags": only the deploy key can create, update or delete tags. A moved tag would change the hook code every user runs.
- Ruleset "Protect main": no deletion, no force push.

## When the build fails after the tag

The tag and the release already exist. Re-run the failed jobs from the Actions run: the upload uses `--clobber`, so running it again replaces the assets without error. Do not delete or move the tag.
