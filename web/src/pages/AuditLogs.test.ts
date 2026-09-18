// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { buildOperationOptions } from './AuditLogs'

describe('audit operation filters', () => {
  test('offers plugin permission and activity filters', () => {
    const options = buildOperationOptions()
    expect(
      options.find(
        (option) => option.value === 'EXTERNAL_PLUGIN_HOST_OPERATION_DENIED'
      )?.group
    ).toBe('Plugins')
    expect(
      options.find(
        (option) => option.value === 'EXTERNAL_PLUGIN_GRANTS_CHANGED'
      )?.label
    ).toBe('Plugin Permissions Changed')
  })
  test('offers permission denials in the authentication group', () => {
    expect(
      buildOperationOptions().find(
        (option) => option.value === 'PERMISSION_DENIED'
      )
    ).toEqual({
      value: 'PERMISSION_DENIED',
      label: 'Permission Denied',
      group: 'Authentication',
      keywords: 'PERMISSION_DENIED',
    })
  })
})
