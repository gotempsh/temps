// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  canManageExternalPlugins,
  pluginReloadFailures,
} from './plugin-registry'

describe('pluginReloadFailures', () => {
  test('retains per-plugin reasons from partial and failed reload bodies', () => {
    const failures = [
      { plugin: 'deployment-pulse', reason: 'Signature revoked' },
    ]
    expect(pluginReloadFailures({ loaded: 1, failures })).toEqual(failures)
    expect(pluginReloadFailures({ loaded: 0, failures })).toEqual(failures)
  })

  test('handles network errors, problem details, and malformed failure entries', () => {
    for (const value of [
      undefined,
      null,
      new Error('offline'),
      { detail: 'Forbidden' },
      { failures: 'invalid' },
    ]) {
      expect(pluginReloadFailures(value)).toEqual([])
    }
    expect(
      pluginReloadFailures({ failures: [null, {}, { plugin: 'x', reason: 3 }] })
    ).toEqual([])
  })

  test('retains registry-wide failures without a plugin name', () => {
    const failures = [
      { plugin: null, reason: 'Cannot read plugin directory' },
      { reason: 'Invalid registry state' },
    ]
    expect(pluginReloadFailures({ failures })).toEqual(failures)
  })
})

describe('canManageExternalPlugins', () => {
  test('matches the roles that carry the backend SystemAdmin permission', () => {
    expect(canManageExternalPlugins('admin')).toBe(true)
    expect(canManageExternalPlugins('platform_admin')).toBe(true)
    expect(canManageExternalPlugins('user')).toBe(false)
    expect(canManageExternalPlugins('reader')).toBe(false)
    expect(canManageExternalPlugins(null)).toBe(false)
  })
})
