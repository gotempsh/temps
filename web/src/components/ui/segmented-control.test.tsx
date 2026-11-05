// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { SegmentedControl } from './segmented-control'

installBrowserWindow()
afterEach(cleanup)

const OPTIONS = [
  { value: 'latency', label: 'Latency' },
  { value: 'traffic', label: 'Traffic' },
] as const

function Harness() {
  const [value, setValue] = useState<'latency' | 'traffic'>('latency')
  return (
    <SegmentedControl
      label="Chart metric"
      value={value}
      onValueChange={setValue}
      options={OPTIONS}
    />
  )
}

test('one option is chosen, and only it is in the tab order', () => {
  render(<Harness />)
  const group = screen.getByRole('radiogroup', { name: 'Chart metric' })
  expect(group).toBeTruthy()
  const latency = screen.getByRole('radio', { name: 'Latency' })
  const traffic = screen.getByRole('radio', { name: 'Traffic' })
  expect(latency.getAttribute('aria-checked')).toBe('true')
  expect(traffic.getAttribute('aria-checked')).toBe('false')
  expect(latency.tabIndex).toBe(0)
  expect(traffic.tabIndex).toBe(-1)
})

test('clicking and arrow keys change the choice', () => {
  render(<Harness />)
  fireEvent.click(screen.getByRole('radio', { name: 'Traffic' }))
  expect(
    screen.getByRole('radio', { name: 'Traffic' }).getAttribute('aria-checked')
  ).toBe('true')
  fireEvent.keyDown(screen.getByRole('radiogroup'), { key: 'ArrowRight' })
  expect(
    screen.getByRole('radio', { name: 'Latency' }).getAttribute('aria-checked')
  ).toBe('true')
})

test('it sizes to its options instead of scrolling', () => {
  render(<Harness />)
  const group = screen.getByRole('radiogroup')
  expect(group.className).not.toMatch(/overflow|(^|\s)h-\d/)
})
