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

/** Entries that are neither `owner/repo` nor `owner/*`, so they would never match. */
export function invalidConfigRepoEntries(entries: string[]): string[] {
  const segment = /^[A-Za-z0-9._-]+$/
  return entries.filter((entry) => {
    const [owner, repo, ...rest] = entry.split('/')
    if (rest.length > 0 || !owner || !repo) return true
    if (!segment.test(owner) || owner === '.' || owner === '..') return true
    if (repo === '*') return false
    return !segment.test(repo) || repo === '.' || repo === '..'
  })
}
