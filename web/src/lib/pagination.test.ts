// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { clampPage } from './pagination'

describe('pagination', () => {
  test('clamps requested pages to finite list bounds', () => {
    expect(clampPage(20, 5)).toBe(5)
    expect(clampPage(0, 5)).toBe(1)
    expect(clampPage(2.9, 5)).toBe(2)
    expect(clampPage(2, Number.NaN)).toBe(1)
  })
})
