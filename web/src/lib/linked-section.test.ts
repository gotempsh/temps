// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { linkedSectionTargets } from './linked-section'

describe('linkedSectionTargets', () => {
  test('targets the fragment of a section link', () => {
    expect(linkedSectionTargets('', '#vulnerability-scanning')).toEqual([
      'vulnerability-scanning',
    ])
  })

  test('targets a combined settings section from ?section=', () => {
    expect(linkedSectionTargets('?section=build&env=prod', '')).toEqual([
      'settings-section-build',
      'build',
    ])
  })

  test('prefers the fragment when a link carries both', () => {
    expect(linkedSectionTargets('?section=build', '#shield')[0]).toBe('shield')
  })

  test('targets nothing for a plain page link', () => {
    expect(linkedSectionTargets('?env=prod', '')).toEqual([])
    expect(linkedSectionTargets('', '#')).toEqual([])
  })

  test('decodes an encoded fragment and tolerates a malformed one', () => {
    expect(linkedSectionTargets('', '#a%20b')).toEqual(['a b'])
    expect(linkedSectionTargets('', '#%E0%A4%A')).toEqual(['%E0%A4%A'])
  })
})
