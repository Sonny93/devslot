import { readFileSync, writeFileSync } from 'node:fs'

const SEMVER_PATTERN = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/

const MANIFEST_VERSION_PATTERN = /^(\[package\][^[]*?^version = ")([^"]*)(")/m
const LOCK_VERSION_PATTERN = /^(name = "devslot"\nversion = ")([^"]*)(")/m

export function isSemver(version) {
  return SEMVER_PATTERN.test(version)
}

export function readManifestVersion(manifestPath) {
  return MANIFEST_VERSION_PATTERN.exec(readFileSync(manifestPath, 'utf8'))?.[2] ?? null
}

export function readLockVersion(lockPath) {
  return LOCK_VERSION_PATTERN.exec(readFileSync(lockPath, 'utf8'))?.[2] ?? null
}

function replaceVersion(path, pattern, version) {
  const content = readFileSync(path, 'utf8')
  if (!pattern.test(content)) {
    throw new Error(`${path}: no devslot version line found`)
  }
  writeFileSync(path, content.replace(pattern, `$1${version}$3`))
}

export function writeManifestVersion(manifestPath, version) {
  replaceVersion(manifestPath, MANIFEST_VERSION_PATTERN, version)
}

export function writeLockVersion(lockPath, version) {
  replaceVersion(lockPath, LOCK_VERSION_PATTERN, version)
}
