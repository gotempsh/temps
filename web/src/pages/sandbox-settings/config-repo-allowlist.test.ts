// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  configRepoAllowlistProblem,
  formatConfigRepoAllowlist,
  invalidConfigRepoEntries,
  parseConfigRepoAllowlist,
} from './config-repo-allowlist'

test('the list is read one entry per line, trimmed and without duplicates', () => {
  expect(
    parseConfigRepoAllowlist(
      ' org/config \n\nOrg/Config\nplatform/*, other/repo '
    )
  ).toEqual(['org/config', 'platform/*', 'other/repo'])
  expect(parseConfigRepoAllowlist('')).toEqual([])
})

test('the list round-trips through the text box', () => {
  const entries = ['org/config', 'platform/*']
  expect(parseConfigRepoAllowlist(formatConfigRepoAllowlist(entries))).toEqual(
    entries
  )
  expect(formatConfigRepoAllowlist(undefined)).toBe('')
})

test('entries that could never match are reported', () => {
  expect(
    invalidConfigRepoEntries([
      'org/config',
      'platform/*',
      'org',
      'org/a/b',
      '*/*',
      '../x',
      'org/cf g',
    ])
  ).toEqual(['org', 'org/a/b', '*/*', '../x', 'org/cf g'])
})

test('a GitHub URL for a repository is accepted, as the server accepts it', () => {
  expect(
    invalidConfigRepoEntries([
      'https://github.com/org/config',
      'https://github.com/org/config.git',
      'github.com/org/config/',
      'https://user:token@github.com/org/config',
      'https://example.com/org/config',
      'https://github.com/org/*',
    ])
  ).toEqual([
    'https://user:token@github.com/org/config',
    'https://example.com/org/config',
    'https://github.com/org/*',
  ])
})

test('an allow-list with an entry that can never match cannot be saved', () => {
  expect(configRepoAllowlistProblem('org/config\nplatform/*')).toBeNull()
  expect(configRepoAllowlistProblem('')).toBeNull()
  expect(configRepoAllowlistProblem('org/config\norg')).toContain('org is not')
})
