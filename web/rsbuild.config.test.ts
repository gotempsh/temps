// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'vitest'

import { deriveConsoleTarget } from './rsbuild.config'

describe('deriveConsoleTarget', () => {
  it('keeps a non-zero dev slot on its matching Console listener', () => {
    expect(deriveConsoleTarget('http://localhost:8220')).toBe(
      'http://localhost:8221'
    )
  })

  it('preserves the slot-zero default', () => {
    expect(deriveConsoleTarget('http://localhost:8080')).toBe(
      'http://localhost:8081'
    )
  })
})
