// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { deploymentFailureSummary } from './deployment-failure-summary'

describe('deployment failure summary', () => {
  test('preserves literal Rust option strings in application logs', () => {
    const logs =
      'Container logs for unhealthy/stopped services:\nSome("first") then Some("second")'
    expect(
      deploymentFailureSummary(`Job execution failed: crash\n${logs}`)
        .fullReason
    ).toBe(`crash\n${logs}`)
  })

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

  test('cleans a legacy Debug-formatted, doubly prefixed build failure', () => {
    const stored =
      'Job execution failed: Required job \'build_image\' failed: Some("Job execution failed: Failed to build image for linux/arm64: Build failed: Build failed: Docker stream error: process \\"/bin/sh -c make build\\" did not complete successfully: exit code: 2")'

    expect(deploymentFailureSummary(stored).fullReason).toBe(
      'Required job \'build_image\' failed: Failed to build image for linux/arm64: Build failed: process "/bin/sh -c make build" did not complete successfully: exit code: 2'
    )
  })

  test('leaves a clean build failure untouched', () => {
    const reason =
      'Required job \'build_image\' failed: Failed to build image: Build failed: process "/bin/sh -c go build" did not complete successfully: exit code: 1'

    expect(deploymentFailureSummary(reason).fullReason).toBe(reason)
  })

  test('preserves diagnostic prefixes inside container logs', () => {
    const logs =
      'Container logs for unhealthy/stopped services:\nJob execution failed: application task\nDocker stream error: original diagnostic\nBuild failed: Build failed: application text'
    const result = deploymentFailureSummary(
      `Job execution failed: App stopped\n\n${logs}`
    )
    expect(result.summary).toBe('App stopped')
    expect(result.fullReason).toBe(`App stopped\n\n${logs}`)
  })

  test('bounds a long failure even when it has no container-log marker', () => {
    const result = deploymentFailureSummary(`Build failed: ${'x'.repeat(500)}`)

    expect(result.summary.length).toBeLessThanOrEqual(361)
    expect(result.summary.endsWith('…')).toBe(true)
    expect(result.hasMore).toBe(true)
  })
})
