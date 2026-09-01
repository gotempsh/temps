// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  BACKFILL_STALL_THRESHOLD_MS,
  CloudBackfillStatusResponse,
  isBackfillStalled,
} from './cloud-telemetry-backfill'

const NOW = Date.parse('2026-09-01T12:00:00Z')

function status(
  overrides: Partial<CloudBackfillStatusResponse> = {}
): CloudBackfillStatusResponse {
  return {
    project_id: 7,
    status: 'running',
    fidelity: 'queryable',
    backfill_available: true,
    spans_processed: 100,
    spans_total: 1000,
    percent_complete: 10,
    updated_at: new Date(NOW - 1000).toISOString(),
    command: 'temps backfill cloud-telemetry --project 7',
    ...overrides,
  }
}

describe('cloud telemetry backfill stall detection', () => {
  test('a recently updated run is not stalled', () => {
    expect(isBackfillStalled(status(), NOW)).toBe(false)
  })

  test('a run that stopped reporting past the threshold is stalled', () => {
    // The process driving it has almost certainly exited; showing a bar that
    // never moves would leave the operator waiting on nothing.
    const quiet = status({
      updated_at: new Date(NOW - BACKFILL_STALL_THRESHOLD_MS - 1).toISOString(),
    })
    expect(isBackfillStalled(quiet, NOW)).toBe(true)
  })

  test('exactly at the threshold is still considered live', () => {
    const borderline = status({
      updated_at: new Date(NOW - BACKFILL_STALL_THRESHOLD_MS).toISOString(),
    })
    expect(isBackfillStalled(borderline, NOW)).toBe(false)
  })

  test('only a running backfill can stall', () => {
    const old = new Date(NOW - BACKFILL_STALL_THRESHOLD_MS * 10).toISOString()
    for (const state of ['not_started', 'completed', 'failed'] as const) {
      expect(
        isBackfillStalled(status({ status: state, updated_at: old }), NOW)
      ).toBe(false)
    }
  })

  test('a missing or unparsable timestamp never reads as stalled', () => {
    // Guessing "stalled" from missing data would cry wolf on a healthy run.
    expect(isBackfillStalled(undefined, NOW)).toBe(false)
    expect(isBackfillStalled(status({ updated_at: undefined }), NOW)).toBe(false)
    expect(isBackfillStalled(status({ updated_at: 'not a date' }), NOW)).toBe(
      false
    )
  })
})
