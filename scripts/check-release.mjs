// Fails unless package.json, Cargo.toml and Cargo.lock agree on the version
// and CHANGELOG.md has something under "## [Unreleased]".
import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { readLockVersion, readManifestVersion } from './cargo-versions.mjs'

const rootDirectory = fileURLToPath(new URL('..', import.meta.url))
const problems = []

const packageVersion = JSON.parse(readFileSync(`${rootDirectory}package.json`, 'utf8')).version
const manifestVersion = readManifestVersion(`${rootDirectory}Cargo.toml`)
const lockVersion = readLockVersion(`${rootDirectory}Cargo.lock`)
if (packageVersion !== manifestVersion || manifestVersion !== lockVersion) {
  problems.push(
    `versions differ: package.json ${packageVersion}, Cargo.toml ${manifestVersion}, Cargo.lock ${lockVersion}`,
  )
}

function readUnreleasedSection() {
  const changelogPath = `${rootDirectory}CHANGELOG.md`
  if (!existsSync(changelogPath)) return null
  const lines = readFileSync(changelogPath, 'utf8').split('\n')
  const start = lines.findIndex((line) => line.startsWith('## [Unreleased]'))
  if (start === -1) return null
  const length = lines.slice(start + 1).findIndex((line) => line.startsWith('## ['))
  const end = length === -1 ? lines.length : start + 1 + length
  return lines.slice(start + 1, end).join('\n')
}

const unreleased = readUnreleasedSection()
if (unreleased === null) {
  problems.push('CHANGELOG.md has no "## [Unreleased]" section')
} else if (unreleased.trim() === '') {
  problems.push('the "## [Unreleased]" section of CHANGELOG.md is empty')
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`check-release: ${problem}`)
  process.exit(1)
}
console.log(`check-release: ready to release from ${packageVersion}`)
