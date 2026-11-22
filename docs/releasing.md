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
2. The reusable release workflow of minimalstuff/tooling (0.13.0) runs release-it with keep-a-changelog, without the deploy key: it turns `## [Unreleased]` into the version section and bumps `package.json`, `Cargo.toml` and `Cargo.lock`. It then verifies the release commit, rebuilds it as `chore(release): vX.Y.Z` with the bot identity, creates the annotated tag itself, pushes with the deploy key, and creates the GitHub release from the CHANGELOG section, as a draft.
3. A static musl `ds` is built from the release commit SHA the workflow outputs (the build checks that the tag points at it), then packaged: `devslot_<v>_amd64.deb`, `devslot-<v>-1-x86_64.pkg.tar.zst`, `devslot-<v>-x86_64-linux.tar.gz` and `SHA256SUMS`.
4. The upload job downloads the artifact by id, checks the sha256 of `SHA256SUMS` against the one the build job output, runs `sha256sum --check`, checks the directory holds exactly the listed files, then attaches them to the draft release and publishes it.

The release stays a draft, invisible to users, until its packages are attached. A musl smoke build before the tag guards the build step.

## One-time GitHub setup

- Environment `release`, limited to `main`, holding the secret `RELEASE_DEPLOY_KEY`.
- Deploy key "release" with write access to the repository.
- Tag ruleset "Release tags": only the deploy key can create, update or delete tags. A moved tag would change the hook code every user runs. Since tooling 0.12.0 the deploy key is never given to release-it, only to the job that creates the tag.
- Ruleset "Protect main": no deletion, no force push.
- Immutable releases, enabled in the repository settings (Settings, General, Releases). The maintainer's main session does it, once. From then on, a published release's assets and tag can no longer change, even with the deploy key. It needs the draft flow above: assets cannot be added to a published immutable release.

## When the build fails after the tag

The tag and the draft release already exist, and the draft is not visible to users. Re-run the failed jobs from the Actions run: the build runs again from the same release commit, the upload checks the new artifact and uses `--clobber`, so it replaces assets already attached to the draft without error, then publishes. Do not delete or move the tag.

## A published release cannot be fixed

Once the release is published with immutable releases on, its assets and tag are locked. A bad package or a wrong asset is fixed by cutting a new patch release, never by editing the published one.
