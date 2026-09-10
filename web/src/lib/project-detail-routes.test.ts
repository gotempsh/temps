// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { legacyDatabasesRedirectPath } from './project-detail-routes'

describe('legacy project routes', () => {
  test('redirects Databases to the canonical project storage page', () => {
    expect(legacyDatabasesRedirectPath('example')).toBe(
      '/projects/example/storage'
    )
  })
})
