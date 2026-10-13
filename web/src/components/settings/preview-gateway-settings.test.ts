// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { containerNameToSave } from './preview-gateway-settings'

describe('containerNameToSave', () => {
  const savedDefault = {
    container_name: 'temps-preview-gateway',
    default_container_name: 'temps-preview-gateway',
  }

  test('keeps the saved name when the field is unchanged or blank', () => {
    expect(containerNameToSave('temps-preview-gateway', savedDefault)).toBe(
      undefined
    )
    expect(containerNameToSave('  ', savedDefault)).toBe(undefined)
    expect(containerNameToSave('temps-preview-gateway-2', null)).toBe(undefined)
  })

  test('sends a new name without surrounding whitespace', () => {
    expect(containerNameToSave(' temps-preview-gateway-2 ', savedDefault)).toBe(
      'temps-preview-gateway-2'
    )
  })

  test('a blank field restores the default name', () => {
    expect(
      containerNameToSave('', {
        ...savedDefault,
        container_name: 'temps-preview-gateway-2',
      })
    ).toBe('temps-preview-gateway')
  })
})
