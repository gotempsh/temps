// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  issueLabel,
  linkIssueSchema,
  MAX_OWNERSHIP_RULES,
  ownershipRulesSchema,
  patternMatches,
  patternProblem,
  providerName,
  shortSha,
} from './source-control'

describe('linkIssueSchema', () => {
  test('accepts an http(s) URL and trims it', () => {
    const parsed = linkIssueSchema.parse({
      url: '  https://tracker.example.test/browse/SHOP-12 ',
      title: '',
    })
    expect(parsed.url).toBe('https://tracker.example.test/browse/SHOP-12')
  })

  test('refuses other schemes, credentials and empty input', () => {
    for (const url of [
      '',
      'javascript:alert(1)',
      'ftp://tracker.example.test/1',
      'https://user:secret@tracker.example.test/1',
      'not a url',
    ]) {
      expect(linkIssueSchema.safeParse({ url, title: '' }).success).toBe(false)
    }
  })
})

describe('ownership patterns', () => {
  test('match like the server: directories, file names and **', () => {
    expect(patternMatches('src/billing/', 'src/billing/invoice/pdf.ts')).toBe(
      true
    )
    expect(patternMatches('src/billing/', 'src/billingx/a.ts')).toBe(false)
    expect(patternMatches('*.py', 'app/models/cart.py')).toBe(true)
    expect(patternMatches('*.py', 'app/models/cart.pyc')).toBe(false)
    expect(patternMatches('src/**/cart.ts', 'src/cart.ts')).toBe(true)
    expect(patternMatches('src/**/cart.ts', 'src/shop/v2/cart.ts')).toBe(true)
    expect(patternMatches('src/*/cart.ts', 'src/shop/v2/cart.ts')).toBe(false)
    expect(patternMatches('lib/pay?.rb', 'lib/pay1.rb')).toBe(true)
    expect(patternMatches('a.b', 'axb')).toBe(false)
  })

  test('say what is wrong with a pattern', () => {
    expect(patternProblem('src/billing/')).toBeNull()
    expect(patternProblem('')).toContain('Enter a path')
    expect(patternProblem('src\\a')).toContain("'/'")
    expect(patternProblem('src/../etc')).toContain("'..'")
  })

  test('the rules form needs an owner and allows at most 50 rules', () => {
    const rule = { pattern: 'src/a/', owner_user_id: 4, owner_label: 'Ana' }
    expect(ownershipRulesSchema.safeParse({ rules: [rule] }).success).toBe(true)
    expect(
      ownershipRulesSchema.safeParse({
        rules: [{ ...rule, owner_user_id: 0 }],
      }).success
    ).toBe(false)
    expect(
      ownershipRulesSchema.safeParse({
        rules: Array.from({ length: MAX_OWNERSHIP_RULES + 1 }, () => rule),
      }).success
    ).toBe(false)
  })
})

describe('labels', () => {
  test('name issues, commits and providers', () => {
    expect(
      issueLabel({
        provider: 'github',
        repository: 'acme/shop',
        number: 12,
        url: 'https://github.com/acme/shop/issues/12',
      })
    ).toBe('acme/shop#12')
    expect(
      issueLabel({ provider: 'link', url: 'https://tracker.example.test/1' })
    ).toBe('tracker.example.test')
    expect(shortSha('c0ffee1234')).toBe('c0ffee1')
    expect(providerName('gitlab')).toBe('GitLab')
    expect(providerName(undefined)).toBe('the repository')
  })
})
