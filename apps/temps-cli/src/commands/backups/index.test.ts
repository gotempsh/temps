// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  accumulateCleanupBatch,
  formatBytes,
  maskSecret,
  parseCleanupScheduleId,
  validateCreateScheduleAutomation,
  type RetentionCleanupReport,
} from './index.js'

describe('formatBytes', () => {
  test('scales through units with one decimal place', () => {
    expect(formatBytes(512)).toBe('512.0 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(1024 * 1024 * 3)).toBe('3.0 MB')
  })

  test('renders a missing size as a dash', () => {
    expect(formatBytes(null)).toBe('-')
    expect(formatBytes(undefined)).toBe('-')
  })

  test('keeps a real zero distinct from unknown', () => {
    expect(formatBytes(0)).toBe('0 B')
  })
})

describe('maskSecret', () => {
  test('fully masks values at or under 4 characters', () => {
    // Short enough that showing any of it would leak the whole secret.
    expect(maskSecret('a')).toBe('***')
    expect(maskSecret('abcd')).toBe('***')
  })

  test('keeps a leading and trailing hint for longer values', () => {
    expect(maskSecret('AKIAABCDEFGHIJKL')).toBe('AKIA***KL')
  })
})

describe('validateCreateScheduleAutomation', () => {
  const base = { type: 'full', retention: '30', s3SourceId: '1' }

  test('accepts a well-formed automation input', () => {
    const result = validateCreateScheduleAutomation(base)
    expect(result).toEqual({ ok: true, backupType: 'full', retentionDays: 30, s3SourceId: 1 })
  })

  test('rejects a backup type outside full/incremental', () => {
    // Sending an unsupported type to the API would just 400 with a less
    // helpful message; catching it here keeps the fix a re-read away.
    const result = validateCreateScheduleAutomation({ ...base, type: 'snapshot' })
    expect(result).toEqual({ ok: false, error: 'Invalid backup type: snapshot. Supported: full, incremental' })
  })

  test('rejects a non-positive retention period', () => {
    expect(validateCreateScheduleAutomation({ ...base, retention: '0' })).toEqual({
      ok: false,
      error: 'Invalid retention period',
    })
    expect(validateCreateScheduleAutomation({ ...base, retention: 'nope' })).toEqual({
      ok: false,
      error: 'Invalid retention period',
    })
  })

  test('rejects a non-numeric S3 source id', () => {
    expect(validateCreateScheduleAutomation({ ...base, s3SourceId: 'abc' })).toEqual({
      ok: false,
      error: 'Invalid S3 Source ID',
    })
  })
})

describe('parseCleanupScheduleId', () => {
  test('returns undefined when no schedule filter was given', () => {
    expect(parseCleanupScheduleId(undefined)).toBeUndefined()
  })

  test('parses a positive integer', () => {
    expect(parseCleanupScheduleId('7')).toBe(7)
  })

  test('rejects zero, negative, and non-integer values', () => {
    expect(() => parseCleanupScheduleId('0')).toThrow('--schedule-id must be a positive integer')
    expect(() => parseCleanupScheduleId('-3')).toThrow('--schedule-id must be a positive integer')
    expect(() => parseCleanupScheduleId('abc')).toThrow('--schedule-id must be a positive integer')
  })
})

describe('accumulateCleanupBatch', () => {
  function emptyReport(): RetentionCleanupReport {
    return {
      dry_run: false,
      schedule_id: null,
      expired: 0,
      deleted: 0,
      failed: 0,
      failures: [],
      candidate_backup_ids: [],
      candidate_backup_ids_truncated: false,
    }
  }

  test('sums counters and appends failures across batches', () => {
    const report = emptyReport()
    accumulateCleanupBatch(report, {
      ...emptyReport(),
      expired: 3,
      deleted: 2,
      failed: 1,
      failures: [{ backup_id: 'b1', reason: 'timeout' }],
      candidate_backup_ids: ['b1', 'b2', 'b3'],
    })
    expect(report.expired).toBe(3)
    expect(report.deleted).toBe(2)
    expect(report.failed).toBe(1)
    expect(report.failures).toEqual([{ backup_id: 'b1', reason: 'timeout' }])
    expect(report.candidate_backup_ids).toEqual(['b1', 'b2', 'b3'])
    expect(report.candidate_backup_ids_truncated).toBe(false)
  })

  test('caps the sampled id list at 100 and flags truncation', () => {
    // The report is an operator-facing --json artifact; without the cap it
    // could balloon to the full deleted-id count on a large cleanup.
    const report = emptyReport()
    report.candidate_backup_ids = new Array(98).fill('existing')
    accumulateCleanupBatch(report, {
      ...emptyReport(),
      candidate_backup_ids: ['a', 'b', 'c', 'd'],
    })
    expect(report.candidate_backup_ids.length).toBe(100)
    expect(report.candidate_backup_ids.slice(98)).toEqual(['a', 'b'])
    expect(report.candidate_backup_ids_truncated).toBe(true)
  })

  test('never clears a truncation flag once set by an earlier batch', () => {
    const report = emptyReport()
    report.candidate_backup_ids_truncated = true
    accumulateCleanupBatch(report, { ...emptyReport(), candidate_backup_ids_truncated: false })
    expect(report.candidate_backup_ids_truncated).toBe(true)
  })
})
