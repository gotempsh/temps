// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { fittingTabs } from './page-tab-fit'

// Five tabs of 80px, 20px apart: 480px in all.
const widths = [80, 80, 80, 80, 80]
const strip = { gap: 20, more: 60 }

describe('fittingTabs', () => {
  test('a strip wide enough shows every tab, and no More', () => {
    expect(fittingTabs(widths, { ...strip, available: 480 })).toEqual([
      0, 1, 2, 3, 4,
    ])
  })

  test('a strip that was not measured shows every tab', () => {
    expect(fittingTabs(widths, { ...strip, available: 0 })).toEqual([
      0, 1, 2, 3, 4,
    ])
  })

  test('a narrower strip keeps the tabs that fit beside More', () => {
    // 3 tabs and More: 240 + 60 + 3 gaps = 360.
    expect(fittingTabs(widths, { ...strip, available: 360 })).toEqual([0, 1, 2])
    expect(fittingTabs(widths, { ...strip, available: 359 })).toEqual([0, 1])
  })

  test('the tab shown takes the last place when it falls past what fits', () => {
    expect(
      fittingTabs(widths, { ...strip, available: 360, current: 4 })
    ).toEqual([0, 1, 4])
    // Already among them: nothing moves.
    expect(
      fittingTabs(widths, { ...strip, available: 360, current: 1 })
    ).toEqual([0, 1, 2])
  })

  test('a wider tab shown costs the places it needs', () => {
    expect(
      fittingTabs([80, 80, 80, 80, 200], {
        ...strip,
        available: 360,
        current: 4,
      })
    ).toEqual([4])
  })

  test('a control after the last tab takes its room', () => {
    // 480 for the tabs, and 20 + 70 for the control.
    expect(
      fittingTabs(widths, { ...strip, available: 569, trailing: 70 })
    ).toEqual([0, 1, 2, 3])
    expect(
      fittingTabs(widths, { ...strip, available: 570, trailing: 70 })
    ).toEqual([0, 1, 2, 3, 4])
  })

  test('with room for nothing beside More, the tab shown stays', () => {
    expect(
      fittingTabs(widths, { ...strip, available: 100, current: 3 })
    ).toEqual([3])
    expect(fittingTabs(widths, { ...strip, available: 100 })).toEqual([0])
    expect(fittingTabs([], { ...strip, available: 100 })).toEqual([])
  })
})
