// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { formatBytes, describeRestoreMode, describeRestoreConfirmPrompt } from './restore.js'
import type { RestoreRequestMode } from '../../api/types.gen.js'

describe('formatBytes', () => {
  test('scales through units', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(1024 * 1024 * 3)).toBe('3.0 MB')
  })

  test('treats zero and negative sizes as "0 B" rather than NaN or a negative unit', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(-5)).toBe('0 B')
  })
})

describe('describeRestoreMode', () => {
  test('labels an in-place restore as destructive', () => {
    const mode: RestoreRequestMode = { mode: 'in_place' }
    expect(describeRestoreMode(mode, 'prod-db')).toBe("Restore in place (DESTRUCTIVE) onto 'prod-db'")
  })

  test('labels cloning into a new service as non-destructive', () => {
    const mode: RestoreRequestMode = { mode: 'new_service', name: 'prod-db-clone' }
    expect(describeRestoreMode(mode, 'prod-db', 'prod-db-clone')).toBe(
      "Clone into new service 'prod-db-clone'",
    )
  })

  test('labels PITR into a new service as non-destructive', () => {
    const mode: RestoreRequestMode = {
      mode: 'pitr',
      to_new_service: true,
      target: { kind: 'time', time: '2024-01-01T00:00:00Z' },
    }
    expect(describeRestoreMode(mode, 'prod-db', 'prod-db-pitr')).toBe(
      "Point-in-time recovery → new service 'prod-db-pitr'",
    )
  })

  test('labels in-place PITR as destructive', () => {
    const mode: RestoreRequestMode = {
      mode: 'pitr',
      to_new_service: false,
      target: { kind: 'time', time: '2024-01-01T00:00:00Z' },
    }
    expect(describeRestoreMode(mode, 'prod-db')).toBe(
      "Point-in-time recovery (DESTRUCTIVE) onto 'prod-db'",
    )
  })
})

describe('describeRestoreConfirmPrompt', () => {
  test('warns about overwriting data for an in-place restore', () => {
    const mode: RestoreRequestMode = { mode: 'in_place' }
    expect(describeRestoreConfirmPrompt(mode, 'prod-db')).toBe("This will OVERWRITE data on 'prod-db'. Continue?")
  })

  test('warns about overwriting data for in-place PITR', () => {
    const mode: RestoreRequestMode = {
      mode: 'pitr',
      to_new_service: false,
      target: { kind: 'time', time: '2024-01-01T00:00:00Z' },
    }
    expect(describeRestoreConfirmPrompt(mode, 'prod-db')).toBe("This will OVERWRITE data on 'prod-db'. Continue?")
  })

  test('does not warn about overwrite when restoring to a new service', () => {
    const mode: RestoreRequestMode = { mode: 'new_service', name: 'prod-db-clone' }
    expect(describeRestoreConfirmPrompt(mode, 'prod-db')).toBe('Proceed with restore?')
  })

  test('does not warn about overwrite for PITR routed to a new service', () => {
    const mode: RestoreRequestMode = {
      mode: 'pitr',
      to_new_service: true,
      target: { kind: 'time', time: '2024-01-01T00:00:00Z' },
    }
    expect(describeRestoreConfirmPrompt(mode, 'prod-db')).toBe('Proceed with restore?')
  })
})
