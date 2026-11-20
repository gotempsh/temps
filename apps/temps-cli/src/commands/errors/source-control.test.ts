// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { issueLabel, MAX_OWNERSHIP_RULES, parseOwnershipRules } from './source-control'

describe('parseOwnershipRules', () => {
  test('reads one pattern and owner per line, in order, without comments', () => {
    const rules = parseOwnershipRules(`# billing owns its code
src/billing/   4

*.py 7   # data team
`)
    expect(rules).toEqual([
      { pattern: 'src/billing/', owner_user_id: 4 },
      { pattern: '*.py', owner_user_id: 7 },
    ])
  })

  test('names the line that is wrong', () => {
    expect(() => parseOwnershipRules('src/a/ 4\nsrc/b/')).toThrow('Line 2')
    expect(() => parseOwnershipRules('src/a/ ana')).toThrow('user ID')
    expect(() => parseOwnershipRules('src/a/ 4 extra')).toThrow('Line 1')
  })

  test('refuses more rules than a project may have', () => {
    const text = Array.from({ length: MAX_OWNERSHIP_RULES + 1 }, (_, i) => `src/${i}/ 4`).join('\n')
    expect(() => parseOwnershipRules(text)).toThrow(`at most ${MAX_OWNERSHIP_RULES}`)
  })
})

describe('issueLabel', () => {
  test('names a repository issue by number and any other by host', () => {
    expect(
      issueLabel({ provider: 'gitlab', repository: 'acme/shop', number: 3, url: 'https://git.example.test/acme/shop/-/issues/3' }),
    ).toBe('acme/shop#3')
    expect(issueLabel({ provider: 'link', repository: null, number: null, url: 'https://tracker.example.test/SHOP-1' })).toBe(
      'tracker.example.test',
    )
  })
})
