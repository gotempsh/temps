// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { isSandboxExpired, type SandboxView } from './helpers'

const NOW = Date.parse('2026-09-03T12:00:00Z')

function sandbox(overrides: Partial<SandboxView> = {}): SandboxView {
  return {
    id: 'sbx_test',
    name: 'test',
    status: 'stopped',
    image: null,
    work_dir: '/workspace',
    created_at: '2026-09-03T10:00:00Z',
    expires_at: '2026-09-03T11:00:00Z',
    preview_url_template: '',
    ...overrides,
  }
}

describe('isSandboxExpired', () => {
  test('keeps an idle persistent workspace out of the expired bucket', () => {
    expect(isSandboxExpired(sandbox({ lifecycle: 'workspace' }), NOW)).toBe(
      false
    )
  })

  test('expires an ephemeral sandbox after its deadline', () => {
    expect(isSandboxExpired(sandbox({ lifecycle: 'ephemeral' }), NOW)).toBe(
      true
    )
  })

  test('keeps explicitly destroyed workspaces in the audit view', () => {
    expect(
      isSandboxExpired(
        sandbox({ lifecycle: 'workspace', status: 'destroyed' }),
        NOW
      )
    ).toBe(true)
  })
})
