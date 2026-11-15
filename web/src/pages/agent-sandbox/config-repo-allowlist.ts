// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The allow-list of config repositories is edited as one entry per line
 * (`owner/repo`, or `owner/*` for every repository of one owner).
 */
export function parseConfigRepoAllowlist(text: string): string[] {
  const seen = new Set<string>()
  const entries: string[] = []
  for (const line of text.split(/[\n,]/)) {
    const entry = line.trim()
    if (!entry || seen.has(entry.toLowerCase())) continue
    seen.add(entry.toLowerCase())
    entries.push(entry)
  }
  return entries
}

export function formatConfigRepoAllowlist(
  entries: string[] | undefined
): string {
  return (entries ?? []).join('\n')
}

const SEGMENT = /^[A-Za-z0-9._-]+$/

function validSegment(segment: string): boolean {
  return SEGMENT.test(segment) && segment !== '.' && segment !== '..'
}

/**
 * Whether `entry` names one repository the way the server reads it
 * (`parse_repo_ref` in `temps-core`): `owner/repo`, optionally written as a
 * `https://github.com/owner/repo(.git)` URL.
 */
function isRepoRef(entry: string): boolean {
  let rest = entry.trim()
  for (const prefix of [
    'https://github.com/',
    'http://github.com/',
    'github.com/',
  ]) {
    if (rest.startsWith(prefix)) {
      rest = rest.slice(prefix.length)
      break
    }
  }
  rest = rest.replace(/\/+$/, '')
  if (rest.endsWith('.git')) rest = rest.slice(0, -'.git'.length)
  const [owner, repo, ...extra] = rest.split('/')
  return (
    extra.length === 0 &&
    owner !== undefined &&
    repo !== undefined &&
    validSegment(owner) &&
    validSegment(repo)
  )
}

/**
 * Entries the server would never match: neither a repository (see
 * `isRepoRef`) nor `owner/*`.
 */
export function invalidConfigRepoEntries(entries: string[]): string[] {
  return entries.filter((entry) => {
    const trimmed = entry.trim()
    if (trimmed.endsWith('/*')) return !validSegment(trimmed.slice(0, -2))
    return !isRepoRef(trimmed)
  })
}
