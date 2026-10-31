// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { uptimeStart } from './container-uptime'

describe('uptimeStart', () => {
  test('a running container counts from its start', () => {
    expect(uptimeStart('running', '2026-01-02T03:04:05Z')).toBe(
      '2026-01-02T03:04:05Z'
    )
  })

  test('an exited or stopped container shows no uptime', () => {
    for (const status of ['exited', 'stopped', 'paused', 'created']) {
      expect(uptimeStart(status, '2026-01-02T03:04:05Z')).toBeNull()
    }
  })

  test('nothing to count from shows no uptime', () => {
    expect(uptimeStart('running', null)).toBeNull()
    expect(uptimeStart(undefined, undefined)).toBeNull()
  })
})
