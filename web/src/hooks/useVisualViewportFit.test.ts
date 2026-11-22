// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { viewportFit } from './useVisualViewportFit'

describe('viewportFit', () => {
  test('is the visible height and offset, rounded to whole pixels', () => {
    expect(viewportFit({ height: 412.4, offsetTop: 0 })).toEqual({
      height: 412,
      top: 0,
    })
    expect(viewportFit({ height: 500, offsetTop: 63.6 })).toEqual({
      height: 500,
      top: 64,
    })
  })

  test('never reports a negative size or offset', () => {
    expect(viewportFit({ height: -3, offsetTop: -8 })).toEqual({
      height: 0,
      top: 0,
    })
  })
})
