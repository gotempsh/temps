// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  pluginInstallAction,
  safeRegistryNavigationUrl,
} from './plugin-registry'

describe('safeRegistryNavigationUrl', () => {
  test('accepts absolute HTTP and HTTPS registry metadata', () => {
    expect(
      safeRegistryNavigationUrl('https://github.com/gotempsh/plugins')
    ).toBe('https://github.com/gotempsh/plugins')
  })

  test('rejects script, data, credential, and relative targets', () => {
    expect(
      safeRegistryNavigationUrl('javascript:alert(document.cookie)')
    ).toBeUndefined()
    expect(
      safeRegistryNavigationUrl('data:image/svg+xml,<svg/>')
    ).toBeUndefined()
    expect(
      safeRegistryNavigationUrl('https://token@example.com/plugin')
    ).toBeUndefined()
    expect(safeRegistryNavigationUrl('//evil.example/plugin')).toBeUndefined()
    expect(safeRegistryNavigationUrl('/relative/plugin')).toBeUndefined()
  })
})

describe('pluginInstallAction', () => {
  test('offers installation, recognizes the active release, and permits upgrades', () => {
    expect(pluginInstallAction(undefined, '1.0.0')).toBe('install')
    expect(pluginInstallAction('1.0.0', '1.0.0')).toBe('installed')
    expect(pluginInstallAction('1.0.0', '1.1.0')).toBe('upgrade')
  })
})
