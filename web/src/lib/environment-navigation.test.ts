// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  resolveEnvironmentId,
  resolveEnvironmentSelection,
  resolveEnvironmentView,
  updateEnvironmentSearchParams,
} from './environment-navigation'

describe('environment navigation', () => {
  test('falls back to the first environment when the requested one is invalid', () => {
    expect(resolveEnvironmentId('42', [7, 9])).toBe(7)
    expect(resolveEnvironmentId(null, [7, 9])).toBe(7)
    expect(resolveEnvironmentId('9', [7, 9])).toBe(9)
    expect(resolveEnvironmentId(null, [])).toBeUndefined()
  })

  test('only accepts known environment views', () => {
    expect(resolveEnvironmentView('settings')).toBe('settings')
    expect(resolveEnvironmentView('containers')).toBe('containers')
    expect(resolveEnvironmentView('unknown')).toBe('containers')
  })

  test('accepts an environment name in a deep link', () => {
    const environments = [
      { id: 7, name: 'Production' },
      { id: 9, name: 'Preview' },
    ]

    expect(resolveEnvironmentSelection('production', environments)).toBe(7)
    expect(resolveEnvironmentSelection('PREVIEW', environments)).toBe(9)
    expect(resolveEnvironmentSelection('9', environments)).toBe(9)
  })

  test('preserves the selected environment while switching views', () => {
    const current = new URLSearchParams(
      'environment=9&view=settings&source=tour'
    )
    const next = updateEnvironmentSearchParams(current, { view: 'containers' })

    expect(next.get('environment')).toBe('9')
    expect(next.has('view')).toBe(false)
    expect(next.get('source')).toBe('tour')
  })

  test('updates the environment without losing the active settings view', () => {
    const current = new URLSearchParams('environment=7&view=settings')
    const next = updateEnvironmentSearchParams(current, { environmentId: 9 })

    expect(next.toString()).toBe('environment=9&view=settings')
  })
})
