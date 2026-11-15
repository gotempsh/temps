// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  authorizationRejectedMessage,
  canReauthorizeConnection,
  displayedHealthStatus,
  needsReauthorization,
  providerSupportsReauthorization,
  readReauthorizationCallback,
} from './reauthorization'

describe('Reauthorize action visibility', () => {
  test('OAuth GitHub and GitLab connections expose the menu action', () => {
    for (const provider_type of ['github', 'gitlab']) {
      expect(
        canReauthorizeConnection(
          { installation_id: null, user_id: 4 },
          { auth_method: 'oauth', provider_type },
          4
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
          { installation_id: null, user_id: 4 },
          { auth_method, provider_type: 'github' },
          4
        )
      ).toBe(false)
    }
    expect(
      canReauthorizeConnection(
        { installation_id: '123', user_id: 4 },
        { auth_method: 'oauth', provider_type: 'github' },
        4
      )
    ).toBe(false)
    expect(
      canReauthorizeConnection({ installation_id: null, user_id: 4 })
    ).toBe(false)
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

describe('Rejected authorization message', () => {
  const gitlab = { auth_method: 'oauth', provider_type: 'gitlab' }
  const message = "GitLab no longer accepts this connection's authorization"

  test('a blocked GitLab OAuth connection gets the fixed message', () => {
    expect(
      authorizationRejectedMessage(
        { is_expired: true, health_message: null },
        gitlab
      )
    ).toBe(message)
  })

  test('a raw response stored by an older server is replaced', () => {
    expect(
      authorizationRejectedMessage(
        {
          is_expired: false,
          health_message:
            'Failed to refresh token: 400 Bad Request - {"error":"invalid_grant"}',
        },
        gitlab
      )
    ).toBe(message)
  })

  test('other failures and connections keep their own message', () => {
    expect(
      authorizationRejectedMessage(
        { is_expired: false, health_message: 'Provider error: HTTP 503' },
        gitlab
      )
    ).toBeNull()
    expect(
      authorizationRejectedMessage(
        { is_expired: true, health_message: null },
        { auth_method: 'gitlab_pat', provider_type: 'gitlab' }
      )
    ).toBeNull()
    expect(
      authorizationRejectedMessage(
        { is_expired: true, health_message: null },
        { auth_method: 'oauth', provider_type: 'github' }
      )
    ).toBeNull()
    expect(
      authorizationRejectedMessage({ is_expired: true, health_message: null })
    ).toBeNull()
  })
})

describe('providerSupportsReauthorization', () => {
  test('only OAuth GitHub and GitLab providers have the action', () => {
    for (const provider_type of ['github', 'gitlab']) {
      expect(
        providerSupportsReauthorization({ auth_method: 'oauth', provider_type })
      ).toBe(true)
    }
    for (const provider of [
      { auth_method: 'gitlab_app', provider_type: 'gitlab' },
      { auth_method: 'gitlab_pat', provider_type: 'gitlab' },
      { auth_method: 'oauth', provider_type: 'gitea' },
      undefined,
    ]) {
      expect(providerSupportsReauthorization(provider)).toBe(false)
    }
  })
})

describe('displayedHealthStatus', () => {
  test('an expired connection is unhealthy whatever the last check said', () => {
    for (const health_status of ['healthy', 'unhealthy', 'unknown']) {
      expect(displayedHealthStatus({ is_expired: true, health_status })).toBe(
        'unhealthy'
      )
    }
  })

  test('otherwise the last check stands', () => {
    for (const health_status of ['healthy', 'unhealthy', 'unknown']) {
      expect(displayedHealthStatus({ is_expired: false, health_status })).toBe(
        health_status
      )
    }
  })
})
