// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  composePreviewEndpoint,
  composePreviewErrorClass,
  composePreviewErrorMessage,
  composePreviewFailedCheck,
  composePreviewPolicyCheck,
  isPublicRepositoryRateLimitError,
} from './compose-preview'

describe('Compose preview routing', () => {
  test('uses the authenticated repository endpoint for private repositories', () => {
    expect(
      composePreviewEndpoint({ kind: 'connected', repositoryId: 42 })
    ).toEqual({
      url: '/repositories/{repository_id}/compose-file/preview',
      path: { repository_id: 42 },
    })
  })

  test('uses the provider endpoint only for public repositories', () => {
    expect(
      composePreviewEndpoint({
        kind: 'public',
        provider: 'github',
        owner: 'docker',
        repo: 'awesome-compose',
      })
    ).toEqual({
      url: '/git/public/{provider}/{owner}/{repo}/compose-file',
      path: {
        provider: 'github',
        owner: 'docker',
        repo: 'awesome-compose',
      },
    })
  })
})

describe('Compose preview errors', () => {
  test('explains that a 405 comes from an outdated running backend', () => {
    expect(composePreviewErrorMessage({ status: 405 })).toContain(
      'Restart or update the backend'
    )
  })

  test('preserves Problem Details returned by the backend', () => {
    expect(
      composePreviewErrorMessage({
        status: 400,
        problem: { detail: 'Compose override is invalid' },
      })
    ).toBe('Compose override is invalid')
  })

  test('identifies the exact check to review for a policy denial', () => {
    expect(
      composePreviewPolicyCheck({
        status: 400,
        problem: { policy_check: 'extends', detail: 'Blocked extends' },
      })
    ).toBe('extends')
    expect(composePreviewPolicyCheck({ status: 400 })).toBeNull()
  })

  test('recognizes a provider rate limit by HTTP status', () => {
    expect(isPublicRepositoryRateLimitError({ status: 429 })).toBe(true)
    expect(isPublicRepositoryRateLimitError({ status: 403 })).toBe(false)
    expect(isPublicRepositoryRateLimitError(null)).toBe(false)
  })
})

describe('Compose preview deploy check', () => {
  const failedCheck = {
    passed: false,
    error_class: 'policy_violation' as const,
    error:
      "Compose security policy rejected privileged for service 'web': privileged containers are not allowed",
    resolution_reasons: [],
    resolution_ran: false,
    command: null,
    unset_variables: [],
    referenced_variables: ['APP_PORT'],
    notes: [],
  }

  test('exposes the deploy error class and the failed check', () => {
    const error = {
      status: 400,
      problem: {
        title: 'Compose Check Failed',
        detail: 'would fail to deploy',
        error_class: 'policy_violation',
        compose_check: failedCheck,
      },
    }
    expect(composePreviewErrorClass(error)).toBe('policy_violation')
    expect(composePreviewFailedCheck(error)).toEqual(failedCheck)
    expect(composePreviewErrorMessage(error)).toBe('would fail to deploy')
  })

  test('is empty for a preview error without a check', () => {
    const error = { status: 400, problem: { title: 'Invalid', detail: 'x' } }
    expect(composePreviewErrorClass(error)).toBeNull()
    expect(composePreviewFailedCheck(error)).toBeNull()
    expect(composePreviewFailedCheck(null)).toBeNull()
  })
})
