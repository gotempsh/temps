// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { Slider } from './slider'

describe('Slider', () => {
  test('forwards aria-label to the focusable thumb, not just the root', () => {
    const markup = renderToStaticMarkup(
      <Slider
        id="threshold-slider"
        aria-label="Disk space alert threshold percentage"
        value={[80]}
        min={50}
        max={99}
        step={1}
      />
    )

    // Radix renders role="slider" on the Thumb — the accessible name must
    // land on that element, not only on the Root (which merely carries the
    // `id` an htmlFor label would otherwise, uselessly, point at).
    const thumbMarkup = markup.slice(markup.indexOf('role="slider"'))
    expect(thumbMarkup.slice(0, 200)).toContain(
      'aria-label="Disk space alert threshold percentage"'
    )
  })
})
