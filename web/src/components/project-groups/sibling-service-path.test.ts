// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { siblingServicePath } from './sibling-service-path'

describe('siblingServicePath', () => {
  test('keeps the section being viewed', () => {
    expect(siblingServicePath('/projects/web', 'web', 'api')).toBe('/projects/api')
    expect(siblingServicePath('/projects/web/settings/environment-variables', 'web', 'api')).toBe(
      '/projects/api/settings/environment-variables'
    )
  })

  test('stops before a record that belongs to the service being left', () => {
    expect(siblingServicePath('/projects/web/deployments/42', 'web', 'api')).toBe(
      '/projects/api/deployments'
    )
    expect(
      siblingServicePath(
        '/projects/web/monitors/0b7c2a1e-3f4d-4c5b-9a8e-1d2c3b4a5f6e/edit',
        'web',
        'api'
      )
    ).toBe('/projects/api/monitors')
  })

  test('does not treat a slug that merely starts the same as the current one', () => {
    expect(siblingServicePath('/projects/web-admin/settings', 'web', 'api')).toBe('/projects/api')
  })
})
