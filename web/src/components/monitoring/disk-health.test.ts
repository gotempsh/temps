// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NodeDiskHealthResponse } from '@/api/client/types.gen'
import { describeDrive, driveTone, overallTone, summarize } from './disk-health'

const health = (
  overrides: Partial<NodeDiskHealthResponse> = {}
): NodeDiskHealthResponse => ({
  reported: true,
  raid_arrays: [],
  drives: [],
  drives_detected: 0,
  drives_read: 0,
  setup_path: '/monitoring/server',
  ...overrides,
})

describe('driveTone', () => {
  test('media errors outrank wear', () => {
    expect(
      driveTone({ device: 'sda', wear_percent: 10, media_errors: 2 })
    ).toBe('poor')
  })

  test('wear above the alert threshold warns', () => {
    expect(
      driveTone({ device: 'nvme0', wear_percent: 81, media_errors: 0 })
    ).toBe('warn')
    expect(
      driveTone({ device: 'nvme0', wear_percent: 80, media_errors: 0 })
    ).toBe('good')
  })

  test('no SMART data is unknown', () => {
    expect(driveTone({ device: 'sdb' })).toBe('unknown')
  })
})

describe('overallTone', () => {
  test('a degraded array is critical', () => {
    expect(
      overallTone(
        health({
          raid_arrays: [{ name: 'md0', degraded: true, devices_failed: 1 }],
        })
      )
    ).toBe('poor')
  })

  test('nothing found is unknown', () => {
    expect(overallTone(health())).toBe('unknown')
  })
})

describe('summarize', () => {
  test('counts degraded arrays and read drives', () => {
    expect(
      summarize(
        health({
          raid_arrays: [
            { name: 'md0', degraded: true, devices_failed: 1 },
            { name: 'md1', degraded: false, devices_failed: 0 },
          ],
          drives_detected: 2,
          drives_read: 1,
        })
      )
    ).toBe('1 of 2 RAID arrays degraded · 1 of 2 drives read')
  })

  test('says when the node never reported', () => {
    expect(summarize(health({ reported: false }))).toBe('Not reported')
  })
})

describe('describeDrive', () => {
  test('lists wear, errors and lifetime writes', () => {
    expect(
      describeDrive({
        device: 'nvme0',
        wear_percent: 12.4,
        media_errors: 1,
        bytes_written: 2_000_000_000_000,
      })
    ).toMatch(/^12% worn · 1 media error · .+ written$/)
  })
})
