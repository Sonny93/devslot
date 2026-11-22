// Usage: node scripts/set-version.mjs <version>
// Rewrites the devslot version in Cargo.toml and Cargo.lock as text (no cargo).
import { fileURLToPath } from 'node:url'
import {
  isSemver,
  readLockVersion,
  readManifestVersion,
  writeLockVersion,
  writeManifestVersion,
} from './cargo-versions.mjs'

const rootDirectory = fileURLToPath(new URL('..', import.meta.url))
const version = process.argv[2]

function fail(message) {
  console.error(`set-version: ${message}`)
  process.exit(1)
}

if (process.argv.length !== 3 || !isSemver(version)) {
  fail(`expected one semver argument, got ${JSON.stringify(process.argv.slice(2))}`)
}

try {
  if (readManifestVersion(`${rootDirectory}Cargo.toml`) === null) {
    throw new Error('Cargo.toml: no version line in the [package] table')
  }
  if (readLockVersion(`${rootDirectory}Cargo.lock`) === null) {
    throw new Error('Cargo.lock: no version line after name = "devslot"')
  }
  writeManifestVersion(`${rootDirectory}Cargo.toml`, version)
  writeLockVersion(`${rootDirectory}Cargo.lock`, version)
} catch (error) {
  fail(error.message)
}
console.log(`set-version: Cargo.toml and Cargo.lock are now at ${version}`)
