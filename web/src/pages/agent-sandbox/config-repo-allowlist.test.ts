// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
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
