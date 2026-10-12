// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

import { NumberInput } from './number-input'

function render(value: number | null | undefined, extra = {}) {
  return renderToStaticMarkup(
    <NumberInput value={value} onValueChange={() => {}} {...extra} />
  )
}

describe('NumberInput', () => {
  test('renders a native number input showing the committed value', () => {
    const html = render(42, { min: 1, max: 100 })
    expect(html).toContain('type="number"')
    expect(html).toContain('value="42"')
    expect(html).toContain('min="1"')
    expect(html).toContain('max="100"')
  })

  test('an absent value renders an empty field, not 0 or a default', () => {
    expect(render(undefined)).toContain('value=""')
    expect(render(null)).toContain('value=""')
  })

  test('zero renders as 0', () => {
    expect(render(0)).toContain('value="0"')
  })

  test('is a single input element so FormControl can wrap it', () => {
    expect(render(5).match(/<input /g)?.length).toBe(1)
    expect(render(5).startsWith('<input')).toBe(true)
  })

  test('forwards ordinary input props', () => {
    const html = render(3, {
      id: 'cooldown',
      placeholder: '60',
      disabled: true,
    })
    expect(html).toContain('id="cooldown"')
    expect(html).toContain('placeholder="60"')
    expect(html).toContain('disabled=""')
  })
})
