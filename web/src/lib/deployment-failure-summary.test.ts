// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { deploymentFailureSummary } from './deployment-failure-summary'

describe('deployment failure summary', () => {
  test('keeps a short failure unchanged', () => {
    expect(deploymentFailureSummary('Image pull failed')).toEqual({
      fullReason: 'Image pull failed',
      summary: 'Image pull failed',
      hasMore: false,
    })
  })

  test('omits embedded container logs from the default summary', () => {
    const result = deploymentFailureSummary(
      'Compose service exited\\n\\nContainer logs for unhealthy/stopped services:\\n--- app ---\\nfatal: can only run as pid 1'
    )

    expect(result.summary).toBe('Compose service exited')
    expect(result.fullReason).toContain('fatal: can only run as pid 1')
    expect(result.hasMore).toBe(true)
  })

  test('bounds a long failure even when it has no container-log marker', () => {
    const result = deploymentFailureSummary(`Build failed: ${'x'.repeat(500)}`)

    expect(result.summary.length).toBeLessThanOrEqual(361)
    expect(result.summary.endsWith('…')).toBe(true)
    expect(result.hasMore).toBe(true)
  })
})
