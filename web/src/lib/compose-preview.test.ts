import { describe, expect, test } from 'bun:test'

import {
  composePreviewEndpoint,
  composePreviewErrorMessage,
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

  test('recognizes a provider rate limit by HTTP status', () => {
    expect(isPublicRepositoryRateLimitError({ status: 429 })).toBe(true)
    expect(isPublicRepositoryRateLimitError({ status: 403 })).toBe(false)
    expect(isPublicRepositoryRateLimitError(null)).toBe(false)
  })
})
