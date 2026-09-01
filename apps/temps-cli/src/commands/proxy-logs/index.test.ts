// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  formatBytes,
  parseOptionalProjectId,
  summarizeBucketStats,
} from './index.js'

describe('formatBytes', () => {
  test('zero is reported literally, not treated as a log(0) edge case', () => {
    expect(formatBytes(0)).toBe('0 B')
  })

  test('renders sub-KB sizes as whole bytes with no decimal', () => {
    expect(formatBytes(500)).toBe('500 B')
    expect(formatBytes(1023)).toBe('1023 B')
  })

  test('scales through KB, MB and GB with one decimal', () => {
    expect(formatBytes(1024)).toBe('1.0 KB')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB')
    expect(formatBytes(1024 * 1024 * 1024)).toBe('1.0 GB')
  })
})

describe('summarizeBucketStats', () => {
  test('sums requests and errors, and averages response time across buckets', () => {
    const summary = summarizeBucketStats([
      { request_count: 100, error_count: 5, avg_response_time_ms: 50 },
      { request_count: 200, error_count: 15, avg_response_time_ms: 150 },
    ])
    expect(summary.totalRequests).toBe(300)
    expect(summary.totalErrors).toBe(20)
    // Mean of the bucket averages (matches the pretty-printed summary line),
    // not a request-weighted average.
    expect(summary.avgResponseTime).toBe(100)
    expect(summary.errorRatePercent).toBeCloseTo((20 / 300) * 100)
  })

  test('an empty bucket list reports all zeros instead of NaN', () => {
    const summary = summarizeBucketStats([])
    expect(summary).toEqual({
      totalRequests: 0,
      totalErrors: 0,
      avgResponseTime: 0,
      errorRatePercent: 0,
    })
  })

  test('zero total requests keeps the error rate at 0 rather than dividing by zero', () => {
    const summary = summarizeBucketStats([
      { request_count: 0, error_count: 0, avg_response_time_ms: 0 },
    ])
    expect(summary.errorRatePercent).toBe(0)
  })
})

describe('project-scoped detail lookup', () => {
  test('accepts a positive project ID and rejects ambiguous values', () => {
    expect(parseOptionalProjectId(undefined)).toBeUndefined()
    expect(parseOptionalProjectId('42')).toBe(42)
    expect(() => parseOptionalProjectId('0')).toThrow('positive integer')
    expect(() => parseOptionalProjectId('7x')).toThrow('positive integer')
  })
})
