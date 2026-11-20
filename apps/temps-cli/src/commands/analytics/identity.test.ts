// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { describeIdentity, parseIdentityMode } from './identity'

describe('parseIdentityMode', () => {
  test('accepts both modes, case-insensitively', () => {
    expect(parseIdentityMode('persistent')).toBe('persistent')
    expect(parseIdentityMode(' Cookieless ')).toBe('cookieless')
  })

  test('names the accepted values when the mode is unknown', () => {
    expect(() => parseIdentityMode('anonymous')).toThrow(
      '--mode must be one of persistent, cookieless; got "anonymous"',
    )
  })
})

describe('describeIdentity', () => {
  const updated_at = '2026-11-20T10:00:00Z'

  test('persistent mode has no tracker option and no limitations', () => {
    expect(
      describeIdentity({
        project_id: 3,
        mode: 'persistent',
        sdk_option: null,
        limitations: [],
        updated_at,
      }),
    ).toEqual(['Mode: persistent (visitor id kept in a first-party cookie or localStorage)'])
  })

  test('cookieless mode names the tracker option and every limitation', () => {
    const lines = describeIdentity({
      project_id: 3,
      mode: 'cookieless',
      sdk_option: 'cookieless: true',
      limitations: ['A returning visitor counts as a new visitor the next day.'],
      updated_at,
    })
    expect(lines[0]).toContain('cookieless')
    expect(lines[1]).toBe('Tracker option: cookieless: true (script tag: data-cookieless="true")')
    expect(lines[2]).toBe('- A returning visitor counts as a new visitor the next day.')
  })
})
