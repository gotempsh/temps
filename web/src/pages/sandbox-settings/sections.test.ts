// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { sandboxSettingsSection, SANDBOX_SETTINGS_SECTIONS } from './sections'

describe('sandbox settings sections', () => {
  test('the bare settings path opens the runtime section', () => {
    expect(sandboxSettingsSection('/sandboxes/settings')).toBe('runtime')
    expect(sandboxSettingsSection('/sandboxes/settings/')).toBe('runtime')
  })

  test('the preview path opens the preview gateway section', () => {
    expect(sandboxSettingsSection('/sandboxes/settings/preview')).toBe(
      'preview'
    )
    expect(sandboxSettingsSection('/sandboxes/settings/preview/')).toBe(
      'preview'
    )
  })

  test('an unknown sub-path falls back to the runtime section', () => {
    expect(sandboxSettingsSection('/sandboxes/settings/other')).toBe('runtime')
  })

  test('every section links to a path that resolves back to it', () => {
    for (const section of SANDBOX_SETTINGS_SECTIONS) {
      expect(sandboxSettingsSection(section.path)).toBe(section.id)
    }
  })
})
