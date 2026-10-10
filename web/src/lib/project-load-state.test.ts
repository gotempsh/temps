// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { attachHttpStatus } from './http-error-status'
import { projectLoadState } from './project-load-state'

/** A problem body as the client throws it once the status is attached. */
function problem(status: number, title: string) {
  return attachHttpStatus(
    { title, detail: `${title} for project "example-app"` },
    { ok: false, status }
  )
}

const base = { slug: 'example-app', isLoading: false, hasProject: false }

describe('projectLoadState', () => {
  test('only a real 404 says the project does not exist', () => {
    expect(
      projectLoadState({ ...base, error: problem(404, 'Project Not Found') })
    ).toBe('not-found')
  })

  test('a server error is a failed read, not a missing project', () => {
    for (const status of [500, 502, 503]) {
      expect(
        projectLoadState({ ...base, error: problem(status, 'Server Error') })
      ).toBe('failed')
    }
  })

  test('a refused read is a failed read, not a missing project', () => {
    expect(
      projectLoadState({ ...base, error: problem(403, 'Forbidden') })
    ).toBe('failed')
  })

  test('a network error is a failed read', () => {
    expect(
      projectLoadState({ ...base, error: new TypeError('Failed to fetch') })
    ).toBe('failed')
    // A proxy's plain-text error page carries no status either.
    expect(projectLoadState({ ...base, error: 'Bad Gateway' })).toBe('failed')
  })

  test('a 404-looking title without a status is not trusted', () => {
    expect(
      projectLoadState({ ...base, error: { title: 'Not Found', detail: 'x' } })
    ).toBe('failed')
  })

  test('a failed refetch still reports the failure over cached data', () => {
    expect(
      projectLoadState({
        ...base,
        hasProject: true,
        error: problem(500, 'Server Error'),
      })
    ).toBe('failed')
  })

  test('loading, ready and missing-slug states', () => {
    expect(projectLoadState({ ...base, error: null, isLoading: true })).toBe(
      'loading'
    )
    expect(projectLoadState({ ...base, error: null, hasProject: true })).toBe(
      'ready'
    )
    expect(projectLoadState({ ...base, slug: undefined, error: null })).toBe(
      'not-found'
    )
    expect(projectLoadState({ ...base, slug: '', error: null })).toBe(
      'not-found'
    )
  })
})
