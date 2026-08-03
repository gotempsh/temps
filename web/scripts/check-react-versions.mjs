#!/usr/bin/env node
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
 * guarantee. It reads the resolved versions out of bun.lock rather than the
 * declared ranges in package.json, because it is the resolved versions that
 * end up in the bundle.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
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
  console.error(`\n❌ react version check failed\n\n${message}\n`)
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
    fail(`"${name}" is not present in ${lockPath}. Run \`bun install\` in web/.`)
  }
  const specifier = Array.isArray(entry) ? entry[0] : entry
  if (typeof specifier !== 'string') {
    fail(`Unexpected lockfile entry shape for "${name}": ${JSON.stringify(entry)}`)
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
      `\`bun install\` in web/ to refresh bun.lock.`,
  )
}

console.log(`✓ react and react-dom both resolve to ${react}`)
