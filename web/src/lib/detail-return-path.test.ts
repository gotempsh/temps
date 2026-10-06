// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { detailReturnPath, detailReturnState } from './detail-return-path'

const fallback = '/projects/app/environment-variables'

describe('detailReturnPath', () => {
  test('returns to the list that opened the detail page, query included', () => {
    const state = detailReturnState({
      pathname: '/projects/app/settings/variables',
      search: '?section=secrets',
    })
    expect(detailReturnPath(state, 'app', fallback)).toBe(
      '/projects/app/settings/variables?section=secrets'
    )
  })

  test('falls back to the canonical list for a deep link or reload', () => {
    expect(detailReturnPath(null, 'app', fallback)).toBe(fallback)
    expect(detailReturnPath(undefined, 'app', fallback)).toBe(fallback)
    expect(detailReturnPath({ returnTo: 42 }, 'app', fallback)).toBe(fallback)
  })

  test('never leaves the project or the console', () => {
    for (const returnTo of [
      'https://example.com/projects/app/settings',
      '//example.com/projects/app',
      '/projects/other-app/environment-variables',
      '/projects/app-two/environment-variables',
      '/settings/general',
    ]) {
      expect(detailReturnPath({ returnTo }, 'app', fallback)).toBe(fallback)
    }
  })
})
