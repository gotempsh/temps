#!/usr/bin/env bun
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Refresh `openapi.json` from a running temps server, in a canonical shape.
 *
 * Why this exists, and what "canonical" means, is documented once in
 * `openapi-canonical.ts` — this script is the fetching half, `check-openapi.ts`
 * is the verifying half, and both share that definition so they cannot drift.
 *
 * Usage:
 *   bun run scripts/update-openapi.ts                     # localhost:8080
 *   bun run scripts/update-openapi.ts --url http://localhost:8220/api/api-docs/openapi.json
 *   TEMPS_API_KEY=tk_... bun run scripts/update-openapi.ts # if the server requires auth
 *   TEMPS_API_COOKIE='session=...' bun run scripts/update-openapi.ts # browser/session auth
 *
 * Then regenerate the client from the file:
 *   bun run generate:api
 */

import { SPEC_PATH, pathCount, serialize } from './openapi-canonical'

const DEFAULT_URL = 'http://localhost:8080/api/api-docs/openapi.json'

function parseArgs(argv: string[]): { url: string } {
  const index = argv.indexOf('--url')
  if (index !== -1) {
    const url = argv[index + 1]
    if (!url) {
      console.error('--url requires a value')
      process.exit(1)
    }
    return { url }
  }
  return { url: process.env.TEMPS_OPENAPI_URL ?? DEFAULT_URL }
}

const { url } = parseArgs(process.argv.slice(2))
const apiKey = process.env.TEMPS_API_KEY
const apiCookie = process.env.TEMPS_API_COOKIE

const response = await fetch(url, {
  headers:
    apiKey || apiCookie
      ? {
          ...(apiKey ? { Authorization: `Bearer ${apiKey}` } : {}),
          ...(apiCookie ? { Cookie: apiCookie } : {}),
        }
      : {},
}).catch((error: unknown) => {
  console.error(`Could not reach ${url}: ${String(error)}`)
  console.error('Start a temps server first, or pass --url.')
  process.exit(1)
})

if (!response.ok) {
  console.error(`${url} returned ${response.status}`)
  if (response.status === 401 || response.status === 403) {
    console.error(
      'Set TEMPS_API_KEY to an admin key or TEMPS_API_COOKIE to an authenticated session cookie; the spec endpoint is authenticated.',
    )
  }
  process.exit(1)
}

const spec = await response.json()
const paths = pathCount(spec)
if (paths === 0) {
  // A spec with no paths means the server answered but the doc was not
  // assembled — writing it would silently delete the entire committed client.
  console.error('Refusing to write: the fetched spec has no paths.')
  process.exit(1)
}

await Bun.write(SPEC_PATH, serialize(spec))
console.log(`Wrote ${SPEC_PATH} (${paths} paths)`)
console.log('Now run: bun run generate:api')
