// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  templateBelongsToSource,
  templateSource,
} from './template-source-selection'

describe('template source selection', () => {
  test('service selections belong only to the Services source', () => {
    const template = { kind: 'service' as const }

    expect(templateSource(template)).toBe('services')
    expect(templateBelongsToSource(template, 'services')).toBe(true)
    expect(templateBelongsToSource(template, 'templates')).toBe(false)
  })

  test('starter selections belong only to the Template source', () => {
    const template = { kind: 'starter' as const }

    expect(templateSource(template)).toBe('templates')
    expect(templateBelongsToSource(template, 'templates')).toBe(true)
    expect(templateBelongsToSource(template, 'services')).toBe(false)
  })
})
