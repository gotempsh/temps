#!/usr/bin/env node
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Fails if the versions React resolves to for `react` and `react-dom` differ.
 *
 * React refuses to boot when the two packages are not on the exact same
 * version: it throws minified error #527 and the console renders a blank white
 * page with nothing but a stack trace in the browser devtools. That is a
 * total outage of the web UI for anyone who installs that build, and there is
 * no server-side signal for it -- `cargo build`, `tsc --noEmit` and `rsbuild
 * build` all succeed on a mismatched tree.
 *
 * This happened in v0.1.0-nightly.20260801 (issue #504): a grouped Dependabot
 * PR bumped react-dom to 19.2.8 and left react on 19.2.7. Dependabot grouping
 * makes that less likely but does not guarantee it -- the two packages were
 * already in the same group when it happened -- so this check is the actual
 * guarantee. It checks both the resolved versions in bun.lock and any
 * workspace-local copies left in node_modules, because either can end up in
 * the bundle.
 */
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const webDir = dirname(dirname(fileURLToPath(import.meta.url)))
const lockPath = join(webDir, 'bun.lock')

/** bun.lock is JSONC (trailing commas, comments); Bun's JSON parser accepts it. */
function readLockfile(path) {
  let raw
  try {
    raw = readFileSync(path, 'utf8')
  } catch (error) {
    fail(`Could not read ${path}: ${error.message}`)
  }
  try {
    // Bun parses JSONC natively; strip trailing commas for plain node.
    return JSON.parse(raw.replace(/,(\s*[}\]])/g, '$1'))
  } catch (error) {
    fail(`Could not parse ${path} as JSON: ${error.message}`)
  }
}

function fail(message) {
  process.stderr.write(`\n❌ react version check failed\n\n${message}\n\n`)
  process.exit(1)
}

const lock = readLockfile(lockPath)
const packages = lock.packages
if (!packages || typeof packages !== 'object') {
  fail(`${lockPath} has no "packages" object -- lockfile format changed?`)
}

/**
 * Entries look like:
 *   "react": ["react@19.2.8", "", {}, "sha512-..."]
 *   "some-pkg/react": ["react@19.1.1", ...]   <- nested copy, keyed by parent
 * We only care about the top-level (hoisted) copies, which are what the app
 * bundle resolves to.
 */
function resolvedVersion(name) {
  const entry = packages[name]
  if (!entry) {
    fail(
      `"${name}" is not present in ${lockPath}. Run \`bun install\` in web/.`
    )
  }
  const specifier = Array.isArray(entry) ? entry[0] : entry
  if (typeof specifier !== 'string') {
    fail(
      `Unexpected lockfile entry shape for "${name}": ${JSON.stringify(entry)}`
    )
  }
  const at = specifier.lastIndexOf('@')
  if (at <= 0) {
    fail(`Could not parse a version out of "${specifier}" for "${name}".`)
  }
  return specifier.slice(at + 1)
}

const react = resolvedVersion('react')
const reactDom = resolvedVersion('react-dom')

if (react !== reactDom) {
  fail(
    `react and react-dom must resolve to the exact same version.\n` +
      `  react:     ${react}\n` +
      `  react-dom: ${reactDom}\n\n` +
      `A mismatch makes the console throw React error #527 at startup and render\n` +
      `a blank page (https://react.dev/errors/527).\n\n` +
      `Fix: set both to the same version in web/package.json, then run\n` +
      `\`bun install\` in web/ to refresh bun.lock.`
  )
}

// Bun can leave a previously installed workspace-local React behind even when
// bun.lock has been updated to a single hoisted version. Imports from that
// workspace then resolve to the stale copy and hooks fail at runtime with an
// "Invalid hook call" even though the lockfile-only check above passes.
const workspaceReactCopies = Object.keys(lock.workspaces ?? {})
  .filter((workspace) => workspace !== '')
  .flatMap((workspace) =>
    ['react', 'react-dom'].map((name) => ({
      name,
      path: join(webDir, workspace, 'node_modules', name, 'package.json'),
    }))
  )
  .filter(({ path }) => existsSync(path))
  .map(({ name, path }) => {
    try {
      return {
        name,
        path,
        version: JSON.parse(readFileSync(path, 'utf8')).version,
      }
    } catch (error) {
      fail(
        `Could not read installed package metadata at ${path}: ${error.message}`
      )
    }
  })

const mismatchedCopies = workspaceReactCopies.filter(
  ({ name, version }) => version !== (name === 'react' ? react : reactDom)
)
if (mismatchedCopies.length > 0) {
  fail(
    `Workspace-local React copies do not match the application runtime:\n` +
      mismatchedCopies
        .map(({ name, path, version }) => `  ${name} ${version}: ${path}`)
        .join('\n') +
      `\n\nRemove the stale workspace node_modules directory and run \`bun install\` ` +
      `in web/. The Rsbuild config also deduplicates React as a runtime safeguard.`
  )
}

process.stdout.write(`✓ react and react-dom both resolve to ${react}\n`)
