#!/usr/bin/env bun
/**
 * Verify that the committed `openapi.json` is in canonical shape.
 *
 * This is the gate that stops the ~92,000-line diff from coming back. It reads
 * only the file on disk — no server, no network, no `bun install` — so the same
 * command runs in a pre-commit hook and in CI on every pull request.
 *
 * Usage:
 *   bun run scripts/check-openapi.ts          # verify, exit 1 if not canonical
 *   bun run scripts/check-openapi.ts --fix    # rewrite it in place
 *
 * `--fix` reformats what is already committed. It does not fetch, so it cannot
 * pick up API changes — use `bun run spec:update` for that.
 */

import { SPEC_PATH, pathCount, serialize } from './openapi-canonical'

const fix = process.argv.slice(2).includes('--fix')
const file = Bun.file(SPEC_PATH)

if (!(await file.exists())) {
  console.error(`Missing ${SPEC_PATH}`)
  process.exit(1)
}

const actual = await file.text()

let spec: unknown
try {
  spec = JSON.parse(actual)
} catch (error) {
  console.error(`${SPEC_PATH} is not valid JSON: ${String(error)}`)
  console.error('Regenerate it with: cd apps/temps-cli && bun run spec:update')
  process.exit(1)
}

const paths = pathCount(spec)
if (paths === 0) {
  console.error(`${SPEC_PATH} declares no paths — that is not a usable spec.`)
  console.error('Regenerate it with: cd apps/temps-cli && bun run spec:update')
  process.exit(1)
}

const expected = serialize(spec)

if (actual === expected) {
  console.log(`openapi.json is canonical (${paths} paths)`)
  process.exit(0)
}

if (fix) {
  await Bun.write(SPEC_PATH, expected)
  console.log(`Reformatted openapi.json (${paths} paths)`)
  console.log('Review the diff, then run: bun run generate:api')
  process.exit(0)
}

// Name the shape of the problem rather than just "differs". The single-line
// case is the one that costs ~92,000 deletions, and it should say so.
const actualLines = actual.split('\n').length
const expectedLines = expected.split('\n').length

console.error(`${SPEC_PATH} is not in canonical shape.`)
if (actualLines <= 2 && expectedLines > 1000) {
  console.error(
    `\nIt is minified: ${actualLines} line(s) where canonical is ${expectedLines}. ` +
      'This is what a raw server response written straight to the file looks like, and it ' +
      `reports ~${expectedLines} deletions, burying the real change.`
  )
} else {
  console.error(
    `\nOn disk: ${actualLines} lines. Canonical: ${expectedLines} lines. ` +
      'Canonical is: keys sorted recursively, two-space indent, trailing newline.'
  )
}
console.error('\nFix it with either:')
console.error('  cd apps/temps-cli && bun run spec:check --fix   # reformat what is committed')
console.error('  cd apps/temps-cli && bun run spec:update        # refetch from a running server')
console.error('then: bun run generate:api')
process.exit(1)
