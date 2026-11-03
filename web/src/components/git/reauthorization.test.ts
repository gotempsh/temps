// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  canReauthorizeConnection,
  needsReauthorization,
  readReauthorizationCallback,
} from './reauthorization'

describe('Reauthorize action visibility', () => {
  test('OAuth GitHub and GitLab connections expose the menu action', () => {
    for (const provider_type of ['github', 'gitlab']) {
      expect(
        canReauthorizeConnection(
          { installation_id: null },
          { auth_method: 'oauth', provider_type }
        )
      ).toBe(true)
    }
  })
  test('PATs, GitHub App installations and unknown providers have no action', () => {
    for (const auth_method of [
      'pat',
      'github_pat',
      'gitlab_pat',
      'github_app',
      'app',
      'ssh',
    ]) {
      expect(
        canReauthorizeConnection(
          { installation_id: null },
          { auth_method, provider_type: 'github' }
        )
      ).toBe(false)
    }
    expect(
      canReauthorizeConnection(
        { installation_id: '123' },
        { auth_method: 'oauth', provider_type: 'github' }
      )
    ).toBe(false)
    expect(canReauthorizeConnection({ installation_id: null })).toBe(false)
  })
  test('expired and unhealthy connections show the direct recovery action', () => {
    expect(
      needsReauthorization({ is_expired: true, health_status: 'healthy' })
    ).toBe(true)
    expect(
      needsReauthorization({ is_expired: false, health_status: 'unhealthy' })
    ).toBe(true)
    expect(
      needsReauthorization({ is_expired: false, health_status: 'healthy' })
    ).toBe(false)
  })
})

test('callback reads only one-time code and state, never a target connection', () => {
  expect(
    readReauthorizationCallback(
      '#reauthorize_state=nonce&reauthorize_code=code%26value&connection_id=999'
    )
  ).toEqual({ state: 'nonce', code: 'code&value' })
  expect(readReauthorizationCallback('#reauthorize_state=nonce')).toEqual({
    state: 'nonce',
    code: '',
  })
  expect(readReauthorizationCallback('#unrelated')).toBeNull()
})
