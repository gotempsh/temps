// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { ViewBars, type ViewBarItem } from './ViewBars'

const bars = (items: ViewBarItem[]) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <ViewBars items={items} />
    </MemoryRouter>
  )

const widths = (markup: string) =>
  [...markup.matchAll(/style="width:([\d.]+)%"/g)].map((match) =>
    Number(match[1])
  )

test('every bar is scaled to the largest, from one baseline', () => {
  const markup = bars([
    { label: 'Direct', value: 2304 },
    { label: 'Organic Search', value: 1152 },
    { label: 'Email', value: 576 },
  ])
  expect(widths(markup)).toEqual([100, 50, 25])
  // Square at the baseline, rounded at the data end, in the one chart hue.
  expect(markup).toContain('rounded-r-[4px] bg-[var(--chart-1)]')
})

test('a row prints its name and figure as text, and the bar is decoration', () => {
  const markup = bars([
    { label: 'Direct', value: 2304, text: '44.9%' },
    { label: 'Email', value: 257 },
  ])
  expect(markup).toContain('>Direct<')
  expect(markup).toContain('>44.9%<')
  // No `text`: the number itself.
  expect(markup).toContain('>257<')
  expect(markup.match(/aria-hidden="true"/g)).toHaveLength(2)
  // The figure is in a text colour, never the bar's.
  expect(markup).not.toMatch(/text-\[var\(--chart/)
})

test('a zero draws no bar, and rows of zeros draw none', () => {
  expect(
    widths(
      bars([
        { label: 'Direct', value: 10 },
        { label: 'Paid Search', value: 0 },
      ])
    )
  ).toEqual([100])
  expect(widths(bars([{ label: 'Direct', value: 0 }]))).toEqual([])
})

test('a name links into the console and nowhere else', () => {
  const markup = bars([
    { label: 'storefront-web', value: 3, href: '/projects/storefront-web' },
    { label: 'elsewhere', value: 2, href: 'https://elsewhere.example' },
  ])
  expect(markup).toContain('href="/projects/storefront-web"')
  expect(markup).not.toContain('elsewhere.example')
})

test('in a narrow chat the bar sits under the name rather than squeezing it', () => {
  const markup = bars([{ label: 'Direct', value: 1 }])
  expect(markup).toContain('@container')
  expect(markup).toContain('col-span-2 row-start-2')
  expect(markup).toContain('@md:col-start-2 @md:row-start-1')
})
