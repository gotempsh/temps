// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { serviceLogAvailability } from '@/lib/service-log-availability'

describe('service log availability', () => {
  test('waits for project links before querying logs', () => {
    expect(
      serviceLogAvailability({
        linksLoading: true,
        linksFailed: false,
        linkedProjectCount: undefined,
      })
    ).toBe('checking')
  })

  test('onboards an unlinked service instead of querying a denied scope', () => {
    expect(
      serviceLogAvailability({
        linksLoading: false,
        linksFailed: false,
        linkedProjectCount: 0,
      })
    ).toBe('needs-project')
  })

  test('loads logs for linked services and defers failed checks to the log API', () => {
    expect(
      serviceLogAvailability({
        linksLoading: false,
        linksFailed: false,
        linkedProjectCount: 1,
      })
    ).toBe('available')
    expect(
      serviceLogAvailability({
        linksLoading: false,
        linksFailed: true,
        linkedProjectCount: undefined,
      })
    ).toBe('available')
  })
})
