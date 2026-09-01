// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { composeSettingPatch } from './compose-settings-patch'

describe('composeSettingPatch', () => {
  test('serializes an empty exclusion list so the last service can be enabled', () => {
    const json = JSON.stringify(
      composeSettingPatch(
        { composePath: 'compose.yaml', excludedServices: ['db'] },
        'excludedServices',
        []
      )
    )

    expect(json).toContain('"excludedServices":[]')
  })

  test('serializes null so an existing Compose override can be cleared', () => {
    const json = JSON.stringify(
      composeSettingPatch(
        { composeOverride: 'services: {}' },
        'composeOverride',
        null
      )
    )

    expect(json).toContain('"composeOverride":null')
  })

  test('serializes an empty sandbox opt-out list so the sandbox can be restored', () => {
    const json = JSON.stringify(
      composeSettingPatch(
        { unsandboxedServices: ['webserver'] },
        'unsandboxedServices',
        []
      )
    )

    expect(json).toContain('"unsandboxedServices":[]')
  })
})
