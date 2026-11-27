// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { isStaleRevision, settingsWriteBody, settingsWriteError } from './settings-revision'

describe('settings writes carry the revision they read', () => {
  it('sends the read revision as the precondition', () => {
    const body = settingsWriteBody(
      { preview_domain: 'a.test', external_url: null, revision: 'abc123' },
      { preview_domain: 'b.test' },
    )
    expect(body.preview_domain).toBe('b.test')
    expect(body.external_url).toBeNull()
    expect(body.expected_revision).toBe('abc123')
  })

  it('writes unconditionally against a server that sends no revision', () => {
    const body = settingsWriteBody({ preview_domain: 'a.test' }, { preview_domain: 'b.test' })
    expect('expected_revision' in body).toBe(false)
  })

  it('explains a refused stale write instead of echoing the problem', () => {
    const refused = {
      title: 'Settings Changed Since They Were Read',
      detail: 'Platform settings changed since it was read',
      expected_revision: 'abc123',
      current_revision: 'def456',
    }
    expect(isStaleRevision(refused)).toBe(true)
    expect(settingsWriteError(refused)).toContain('Run the command again')
    expect(isStaleRevision({ detail: 'Invalid settings' })).toBe(false)
    expect(settingsWriteError({ detail: 'Invalid settings' })).toBe('Invalid settings')
  })
})
