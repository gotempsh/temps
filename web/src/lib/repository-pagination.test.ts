// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { hasNextRepositoryPage } from './repository-pagination'

describe('hasNextRepositoryPage', () => {
  test('does not invent another page when the total exactly fills this page', () => {
    expect(hasNextRepositoryPage(24, 1, 24)).toBe(false)
  })

  test('reports another page when repositories remain', () => {
    expect(hasNextRepositoryPage(25, 1, 24)).toBe(true)
  })

  test('handles later and invalid pages safely', () => {
    expect(hasNextRepositoryPage(48, 2, 24)).toBe(false)
    expect(hasNextRepositoryPage(20, 0, 24)).toBe(false)
    expect(hasNextRepositoryPage(20, 1, 0)).toBe(false)
  })
})
