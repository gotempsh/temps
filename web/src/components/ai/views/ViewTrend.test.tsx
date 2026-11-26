// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, screen } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { LazyViewTrend } from './LazyViewTrend'

installBrowserWindow()
afterEach(cleanup)

const POINTS = [
  { at: 'Nov 23', value: 81 },
  { at: 'Nov 24', value: 112 },
  { at: 'Nov 25', value: 181 },
]

test('a trend loads its chart on demand and names its one series in the caption', async () => {
  const { container } = render(
    <LazyViewTrend label="Visitors per day" points={POINTS} partial />
  )
  // The caption is the legend of a single series.
  expect(await screen.findByText('Visitors per day')).toBeTruthy()
  expect(container.querySelector('figure')).toBeTruthy()
  expect(container.querySelector('figcaption')?.textContent).toBe(
    'Visitors per day'
  )
})

test('too few points say so instead of drawing an empty chart', async () => {
  render(<LazyViewTrend label="Visitors" points={POINTS.slice(0, 1)} />)
  expect(
    await screen.findByText('Not enough points to draw a line')
  ).toBeTruthy()
})
